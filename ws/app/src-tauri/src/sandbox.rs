//! The Workshop's sandbox: start the system the agent built from a blueprint,
//! measure each part through its probe, then stop it all.
//!
//! The commands come from the blueprint file, which the agent wrote; the page
//! shows them and asks before calling this. Requests only ever go to this
//! computer, and redirects are not followed, so a probe cannot be turned into
//! load on somebody else's server.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::ipc::Channel;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

/// How long the system has to come up (a first `docker compose up --build` is slow).
const READY_TIMEOUT: Duration = Duration::from_secs(300);
const STOP_TIMEOUT: Duration = Duration::from_secs(120);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Requests per probe at most, whatever the time and concurrency.
const MAX_REQUESTS: usize = 100_000;
/// The system's own output kept for the report, from the end.
const OUTPUT_KEPT: usize = 6000;

#[derive(Deserialize)]
pub struct Probe {
    method: String,
    url: String,
    body: Option<String>,
}

#[derive(Deserialize)]
pub struct Spec {
    start: String,
    stop: Option<String>,
    dir: Option<String>,
    ready: Option<String>,
    probes: BTreeMap<String, Probe>,
}

/// One part, as measured through its probe.
#[derive(Serialize, Debug, PartialEq)]
pub struct Measured {
    part: String,
    requests: u64,
    failures: u64,
    /// Typical and slow (95th percentile) time per answered request, ms.
    p50: f64,
    p95: f64,
    /// Requests answered per second.
    throughput: f64,
    /// The first failure, to show why.
    error: Option<String>,
}

#[derive(Serialize)]
pub struct Report {
    parts: Vec<Measured>,
    /// The end of what the system printed.
    output: String,
}

/// The one sandbox running: two would fight over the same ports.
static RUNNING: Mutex<Option<CancellationToken>> = Mutex::new(None);

/// Clears RUNNING however the run ends.
struct Running;
impl Drop for Running {
    fn drop(&mut self) {
        if let Ok(mut r) = RUNNING.lock() {
            *r = None;
        }
    }
}

/// `url`, if it points at this computer.
fn local_url(url: &str) -> Result<reqwest::Url, String> {
    let u = reqwest::Url::parse(url).map_err(|e| format!("{url} is not a URL: {e}"))?;
    let host = u
        .host_str()
        .unwrap_or("")
        .trim_start_matches('[')
        .trim_end_matches(']');
    let local = match host.parse::<IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => {
            host.eq_ignore_ascii_case("localhost")
                || host.to_ascii_lowercase().ends_with(".localhost")
        }
    };
    if !matches!(u.scheme(), "http" | "https") || !local {
        return Err(format!(
            "{url}: the sandbox only sends requests to this computer (localhost)"
        ));
    }
    Ok(u)
}

/// The value at `q` (0..1) of sorted `values`.
fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "an index into a short list"
    )]
    let i = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

/// Keeps the end of what a stream prints.
fn keep_output(
    mut from: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    into: Arc<Mutex<String>>,
) {
    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        while let Ok(n) = from.read(&mut buf).await {
            if n == 0 {
                break;
            }
            if let Ok(mut out) = into.lock() {
                out.push_str(&String::from_utf8_lossy(&buf[..n]));
                if out.len() > OUTPUT_KEPT * 2 {
                    let cut = out.len() - OUTPUT_KEPT;
                    let cut = (cut..out.len())
                        .find(|&i| out.is_char_boundary(i))
                        .unwrap_or(cut);
                    out.drain(..cut);
                }
            }
        }
    });
}

fn tail(out: &Arc<Mutex<String>>) -> String {
    let out = out.lock().map(|o| o.clone()).unwrap_or_default();
    let start = out.len().saturating_sub(OUTPUT_KEPT);
    let start = (start..out.len())
        .find(|&i| out.is_char_boundary(i))
        .unwrap_or(0);
    out[start..].to_owned()
}

fn request(
    client: &reqwest::Client,
    method: &str,
    url: &reqwest::Url,
    body: Option<&str>,
) -> Result<reqwest::RequestBuilder, String> {
    let m = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|_| format!("{method} is not an HTTP method"))?;
    let mut r = client.request(m, url.clone());
    if let Some(b) = body {
        r = r
            .header("content-type", "application/json")
            .body(b.to_owned());
    }
    Ok(r)
}

/// What the callers of one probe saw.
#[derive(Default, Clone)]
struct Tally {
    /// Time of each answered request, ms (failed answers too).
    times: Vec<f64>,
    failures: u64,
    /// Requests that got no answer at all.
    unanswered: u64,
    error: Option<String>,
}

/// `concurrency` callers hitting `url` for `time`: how many answered, how fast.
async fn measure(
    client: &reqwest::Client,
    part: &str,
    probe: &Probe,
    url: &reqwest::Url,
    time: Duration,
    concurrency: usize,
    cancel: &CancellationToken,
) -> Result<Measured, String> {
    // Checked once, so a bad method fails here rather than in every caller.
    let _ = request(client, &probe.method, url, None)?;
    let results = Arc::new(Mutex::new(Tally::default()));
    let began = Instant::now();
    let mut callers = tokio::task::JoinSet::new();
    for _ in 0..concurrency {
        let (client, url, results, cancel) =
            (client.clone(), url.clone(), results.clone(), cancel.clone());
        let (method, body) = (probe.method.clone(), probe.body.clone());
        callers.spawn(async move {
            while began.elapsed() < time && !cancel.is_cancelled() {
                if results.lock().map_or(true, |r| {
                    r.times.len() + usize::try_from(r.unanswered).unwrap_or(usize::MAX)
                        >= MAX_REQUESTS
                }) {
                    break;
                }
                let Ok(req) = request(&client, &method, &url, body.as_deref()) else {
                    break;
                };
                let at = Instant::now();
                let outcome = tokio::select! {
                    r = req.send() => r,
                    () = cancel.cancelled() => break,
                };
                let ms = at.elapsed().as_secs_f64() * 1000.0;
                let refused = {
                    let Ok(mut r) = results.lock() else { break };
                    match outcome {
                        Ok(resp) if resp.status().as_u16() < 400 => {
                            r.times.push(ms);
                            false
                        }
                        Ok(resp) => {
                            r.times.push(ms);
                            r.failures += 1;
                            r.error.get_or_insert_with(|| {
                                format!("{} answered {}", method, resp.status())
                            });
                            false
                        }
                        Err(e) => {
                            r.failures += 1;
                            r.unanswered += 1;
                            r.error.get_or_insert_with(|| e.to_string());
                            true
                        }
                    }
                };
                // A refused connection fails at once; do not spin on it.
                if refused {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        });
    }
    while callers.join_next().await.is_some() {}
    let Tally {
        mut times,
        failures,
        unanswered,
        error,
    } = results.lock().map_err(|e| e.to_string())?.clone();
    times.sort_by(f64::total_cmp);
    let answered = times.len() as u64;
    #[allow(
        clippy::cast_precision_loss,
        reason = "request counts are far below 2^52"
    )]
    let throughput = answered as f64 / began.elapsed().as_secs_f64().max(0.001);
    Ok(Measured {
        part: part.to_owned(),
        requests: answered + unanswered,
        failures,
        p50: percentile(&times, 0.5),
        p95: percentile(&times, 0.95),
        throughput,
        error,
    })
}

/// End the start command and everything it started.
async fn kill_tree(child: &mut tokio::process::Child) {
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        let mut k = tokio::process::Command::new("taskkill");
        k.args(["/T", "/F", "/PID", &pid.to_string()])
            .creation_flags(0x0800_0000);
        let _ = k.output().await;
    }
    let _ = child.kill().await;
}

/// Start the system in `dir` (inside `root`), wait until it answers, measure
/// every probe for `seconds` with `concurrency` callers each, then stop it.
#[tauri::command]
pub async fn sandbox_run(
    root: String,
    dir: String,
    spec: Spec,
    seconds: u64,
    concurrency: usize,
    on_progress: Channel<String>,
) -> Result<Report, String> {
    let cancel = CancellationToken::new();
    {
        let mut running = RUNNING.lock().map_err(|e| e.to_string())?;
        if running.is_some() {
            return Err("A sandbox is already running. Stop it first.".to_owned());
        }
        *running = Some(cancel.clone());
    }
    let _running = Running;
    // Everything is checked before anything starts.
    let probes = spec
        .probes
        .iter()
        .map(|(part, p)| local_url(&p.url).map(|u| (part.as_str(), p, u)))
        .collect::<Result<Vec<_>, _>>()?;
    let ready = spec.ready.as_deref().map(local_url).transpose()?;
    if probes.is_empty() && ready.is_none() {
        return Err("The sandbox section names no probes: nothing to measure.".to_owned());
    }
    let rel = [
        dir.trim_matches(['/', '\\']),
        spec.dir.as_deref().unwrap_or("").trim_matches(['/', '\\']),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join("/");
    let cwd = crate::files::resolve(&root, &rel)?;
    let time = Duration::from_secs(seconds.clamp(1, 30));
    let concurrency = concurrency.clamp(1, 64);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;

    let say = |s: String| {
        let _ = on_progress.send(s);
    };
    say(format!("Starting: {}", spec.start));
    let output = Arc::new(Mutex::new(String::new()));
    let mut child = crate::workspace::shell(&spec.start)
        .current_dir(&cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("could not run {}: {e}", spec.start))?;
    if let Some(o) = child.stdout.take() {
        keep_output(o, output.clone());
    }
    if let Some(e) = child.stderr.take() {
        keep_output(e, output.clone());
    }

    let result = async {
        // Up once the ready URL (or else the first probe) gets any answer short of a server error.
        let (check_url, check_method) = match (&ready, probes.first()) {
            (Some(u), _) => (u.clone(), "GET"),
            (None, Some((_, p, u))) => (u.clone(), p.method.as_str()),
            (None, None) => unreachable!("checked above"),
        };
        say(format!("Waiting for {check_url} to answer…"));
        let began = Instant::now();
        loop {
            if cancel.is_cancelled() {
                return Err("Stopped.".to_owned());
            }
            if let Ok(Some(status)) = child.try_wait() {
                if !status.success() {
                    return Err(format!(
                        "The start command ended ({status}) before the system answered."
                    ));
                }
            }
            let probe = request(&client, check_method, &check_url, None)?
                .timeout(Duration::from_secs(3))
                .send()
                .await;
            if probe.is_ok_and(|r| r.status().as_u16() < 500) {
                break;
            }
            if began.elapsed() > READY_TIMEOUT {
                return Err(format!(
                    "{check_url} did not answer within {} s.",
                    READY_TIMEOUT.as_secs()
                ));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let mut parts = Vec::new();
        for (part, p, url) in &probes {
            say(format!(
                "Measuring {part}: {} {url} for {} s, {concurrency} at a time…",
                p.method,
                time.as_secs()
            ));
            parts.push(measure(&client, part, p, url, time, concurrency, &cancel).await?);
            if cancel.is_cancelled() {
                return Err("Stopped.".to_owned());
            }
        }
        Ok(parts)
    }
    .await;

    say("Stopping the system…".to_owned());
    if let Some(stop) = spec.stop.as_deref().filter(|s| !s.trim().is_empty()) {
        let run = crate::workspace::shell(stop)
            .current_dir(&cwd)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        match tokio::time::timeout(STOP_TIMEOUT, run).await {
            Ok(Ok(o)) => {
                if let Ok(mut out) = output.lock() {
                    out.push_str(&String::from_utf8_lossy(&o.stdout));
                    out.push_str(&String::from_utf8_lossy(&o.stderr));
                }
            }
            Ok(Err(e)) => say(format!("Could not run the stop command: {e}")),
            Err(_) => say(format!(
                "The stop command took over {} s; check nothing is left running.",
                STOP_TIMEOUT.as_secs()
            )),
        }
    }
    kill_tree(&mut child).await;
    let out = tail(&output);
    match result {
        Ok(parts) => Ok(Report { parts, output: out }),
        Err(e) if out.trim().is_empty() => Err(e),
        Err(e) => Err(format!("{e}\n\nWhat the system printed:\n{out}")),
    }
}

/// Stop the running sandbox; it then shuts the system down as usual.
#[tauri::command]
pub fn sandbox_stop() -> Result<(), String> {
    if let Some(c) = RUNNING.lock().map_err(|e| e.to_string())?.as_ref() {
        c.cancel();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{Probe, local_url, measure, percentile};
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn requests_only_go_to_this_computer() {
        for ok in [
            "http://localhost:3000/x",
            "http://127.0.0.1/",
            "http://[::1]:80/",
            "https://api.localhost/",
        ] {
            assert!(local_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://example.com/",
            "http://10.0.0.5/",
            "http://localhost.evil.com/",
            "file:///C:/x",
            "ftp://localhost/",
            "nonsense",
        ] {
            assert!(local_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn percentiles_of_sorted_times() {
        let t: Vec<f64> = (1..=100).map(f64::from).collect();
        assert!((percentile(&t, 0.5) - 51.0).abs() < 1e-9);
        assert!((percentile(&t, 0.95) - 95.0).abs() < 1e-9);
        assert!((percentile(&[], 0.5)).abs() < 1e-9);
    }

    /// A tiny local server: GET /ok answers 200, anything else 500.
    async fn server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    let ok = String::from_utf8_lossy(&buf[..n]).starts_with("GET /ok");
                    let status = if ok {
                        "200 OK"
                    } else {
                        "500 Internal Server Error"
                    };
                    let _ = s.write_all(format!("HTTP/1.1 {status}\r\ncontent-length: 2\r\nconnection: close\r\n\r\nhi").as_bytes()).await;
                });
            }
        });
        format!("http://{addr}")
    }

    /// The whole run: start a real server, wait for it, measure it, end it.
    #[tokio::test]
    async fn starts_measures_and_stops_the_system() {
        if std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipped: no Node.js");
            return;
        }
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("server.js"),
            "require('http').createServer((q, r) => r.end('ok')).listen(Number(process.argv[2]), '127.0.0.1');",
        )
        .unwrap();
        let url = format!("http://127.0.0.1:{port}/");
        let spec: super::Spec = serde_json::from_value(serde_json::json!({
            "start": format!("node server.js {port}"),
            "ready": url,
            "probes": { "api": { "method": "GET", "url": url } },
        }))
        .unwrap();
        let progress = tauri::ipc::Channel::new(|_| Ok(()));
        let root = dir.path().display().to_string();
        let report = super::sandbox_run(root, String::new(), spec, 1, 2, progress)
            .await
            .unwrap();
        assert_eq!(report.parts.len(), 1);
        assert!(
            report.parts[0].requests > 0 && report.parts[0].failures == 0,
            "{:?}",
            report.parts
        );
        // Ended with its whole process tree: the port is free again.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            std::net::TcpListener::bind(("127.0.0.1", port)).is_ok(),
            "the server is still running"
        );
    }

    #[tokio::test]
    async fn refuses_to_measure_other_computers() {
        let spec: super::Spec = serde_json::from_value(serde_json::json!({
            "start": "echo never runs",
            "probes": { "api": { "method": "GET", "url": "http://example.com/" } },
        }))
        .unwrap();
        let progress = tauri::ipc::Channel::new(|_| Ok(()));
        let err = super::sandbox_run(".".into(), String::new(), spec, 1, 1, progress)
            .await
            .err()
            .unwrap();
        assert!(
            err.contains("only sends requests to this computer"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn measures_answers_and_failures() {
        let base = server().await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let cancel = CancellationToken::new();
        let probe = |path: &str| Probe {
            method: "GET".into(),
            url: format!("{base}{path}"),
            body: None,
        };
        let good = probe("/ok");
        let m = measure(
            &client,
            "api",
            &good,
            &local_url(&good.url).unwrap(),
            Duration::from_millis(300),
            4,
            &cancel,
        )
        .await
        .unwrap();
        assert!(
            m.requests > 0 && m.failures == 0 && m.error.is_none(),
            "{m:?}"
        );
        assert!(m.throughput > 0.0 && m.p50 <= m.p95);
        let bad = probe("/broken");
        let m = measure(
            &client,
            "api",
            &bad,
            &local_url(&bad.url).unwrap(),
            Duration::from_millis(300),
            2,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(m.failures, m.requests, "{m:?}");
        assert!(
            m.error.as_deref().is_some_and(|e| e.contains("500")),
            "{m:?}"
        );
    }
}
