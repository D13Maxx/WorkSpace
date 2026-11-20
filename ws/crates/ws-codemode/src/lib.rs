//! Code Mode: a model writes a short JavaScript function that searches a tool
//! catalog or calls tools, and it runs here instead of each operation being a
//! tool of its own. Two tools then cover any number of operations, and a
//! script returns only what the model needs from what the tools said.
//!
//! The script runs in QuickJS embedded in the app. It has the language and
//! nothing else: no file system, network, timers, modules or environment. It
//! sees only the globals it is given and, if there is a host, one async
//! function that hands a call back to Rust. Its memory, its running time and
//! its number of calls are capped.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rquickjs::function::{Async, Func};
use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, CaughtError, Promise, Value};

/// What a script may use.
#[derive(Debug, Clone)]
pub struct Limits {
    pub memory_bytes: usize,
    /// From start to finish, waiting on tool calls included.
    pub wall: Duration,
    /// Time spent running JavaScript, so a busy loop stops long before `wall`.
    pub busy: Duration,
    pub max_calls: usize,
    /// Characters of result and of console output, each.
    pub max_output: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_bytes: 256 * 1024 * 1024,
            wall: Duration::from_secs(600),
            busy: Duration::from_secs(30),
            max_calls: 100,
            max_output: 50_000,
        }
    }
}

/// Where a script's calls go: the arguments it passed, as a JSON array.
#[async_trait::async_trait]
pub trait Host: Send + Sync {
    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, String>;
}

/// How a script ended.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// What the function returned, as JSON text; or why it failed.
    pub result: Result<String, String>,
    /// What it wrote with `console.log` and friends.
    pub logs: Vec<String>,
    /// Calls it made to the host.
    pub calls: usize,
}

impl Outcome {
    /// The outcome as one tool result: the logs, then the value or the error.
    #[must_use]
    pub fn report(&self) -> String {
        let mut out = String::new();
        if !self.logs.is_empty() {
            out.push_str("console:\n");
            out.push_str(&self.logs.join("\n"));
            out.push_str("\n\n");
        }
        match &self.result {
            Ok(v) => out.push_str(v),
            Err(e) => {
                out.push_str("error: ");
                out.push_str(e);
            }
        }
        out
    }
}

/// Run `code`, a JavaScript function such as `async () => { ... }`, with
/// `globals` set as global variables. With a host, the script can call
/// `name(...args)`: an async function that resolves to the host's answer and
/// throws its error.
pub async fn run(
    code: &str,
    globals: Vec<(String, serde_json::Value)>,
    host: Option<(String, Arc<dyn Host>)>,
    limits: Limits,
) -> Outcome {
    let calls = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = tokio::sync::oneshot::channel();
    // Tool calls run on the caller's runtime: tool servers live there.
    let here = tokio::runtime::Handle::current();
    let code = code.to_owned();
    let counted = calls.clone();
    let spawned = std::thread::Builder::new()
        .name("code-mode".to_owned())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let outcome = match tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
            {
                Ok(rt) => rt.block_on(sandbox(code, globals, host, limits, here, counted)),
                Err(e) => Outcome {
                    result: Err(format!("could not start the sandbox: {e}")),
                    logs: Vec::new(),
                    calls: 0,
                },
            };
            let _ = tx.send(outcome);
        });
    if let Err(e) = spawned {
        return Outcome {
            result: Err(format!("could not start the sandbox: {e}")),
            logs: Vec::new(),
            calls: 0,
        };
    }
    rx.await.unwrap_or_else(|_| Outcome {
        result: Err("the sandbox stopped without an answer".to_owned()),
        logs: Vec::new(),
        calls: calls.load(Ordering::Relaxed),
    })
}

/// Why the interrupt handler stopped the script, for the error message.
const STOP_NONE: usize = 0;
const STOP_BUSY: usize = 1;
const STOP_WALL: usize = 2;

async fn sandbox(
    code: String,
    globals: Vec<(String, serde_json::Value)>,
    host: Option<(String, Arc<dyn Host>)>,
    limits: Limits,
    here: tokio::runtime::Handle,
    calls: Arc<AtomicUsize>,
) -> Outcome {
    let logs = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let fail = |e: String| Outcome {
        result: Err(e),
        logs: Vec::new(),
        calls: 0,
    };
    let Ok(rt) = AsyncRuntime::new() else {
        return fail("could not start the JavaScript runtime".to_owned());
    };
    rt.set_memory_limit(limits.memory_bytes).await;
    rt.set_max_stack_size(1024 * 1024).await;

    let deadline = Instant::now() + limits.wall;
    let stopped = Arc::new(AtomicUsize::new(STOP_NONE));
    {
        let stopped = stopped.clone();
        let busy_limit = limits.busy;
        let mut busy = Duration::ZERO;
        let mut last = Instant::now();
        rt.set_interrupt_handler(Some(Box::new(move || {
            // Called every few thousand instructions while JavaScript runs.
            // A long gap is time spent waiting on a tool call, not running.
            let now = Instant::now();
            let gap = now - last;
            last = now;
            if gap < Duration::from_millis(100) {
                busy += gap;
            }
            let why = if now > deadline {
                STOP_WALL
            } else if busy > busy_limit {
                STOP_BUSY
            } else {
                STOP_NONE
            };
            if why != STOP_NONE {
                stopped.store(why, Ordering::Relaxed);
            }
            why != STOP_NONE
        })))
        .await;
    }
    let Ok(ctx) = AsyncContext::full(&rt).await else {
        return fail("could not start the JavaScript context".to_owned());
    };

    let max_output = limits.max_output;
    let max_calls = limits.max_calls;
    let work = ctx.async_with(async |ctx| {
        let setup = (|| -> rquickjs::Result<()> {
            let g = ctx.globals();
            for (name, value) in &globals {
                let text = serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned());
                g.set(name.as_str(), ctx.json_parse(text)?)?;
            }
            let sink = logs.clone();
            g.set(
                "__log",
                Func::from(move |line: String| {
                    if let Ok(mut l) = sink.lock() {
                        let used: usize = l.iter().map(String::len).sum();
                        if used < max_output {
                            l.push(line.chars().take(max_output - used).collect());
                        }
                    }
                }),
            )?;
            if let Some((name, host)) = &host {
                let (host, here, calls) = (host.clone(), here.clone(), calls.clone());
                g.set(
                    "__call",
                    Func::from(Async(move |args: String| {
                        let (host, here, calls) = (host.clone(), here.clone(), calls.clone());
                        async move {
                            let reply = if calls.fetch_add(1, Ordering::Relaxed) >= max_calls {
                                Err(format!("a script may make at most {max_calls} calls"))
                            } else {
                                let args = serde_json::from_str(&args)
                                    .unwrap_or(serde_json::Value::Null);
                                let mut task =
                                    Abort(here.spawn(async move { host.call(args).await }));
                                (&mut task.0)
                                    .await
                                    .unwrap_or_else(|e| Err(format!("the call failed: {e}")))
                            };
                            let reply = match reply {
                                Ok(v) => serde_json::json!({ "ok": v }),
                                Err(e) => serde_json::json!({ "error": e }),
                            };
                            rquickjs::Result::Ok(reply.to_string())
                        }
                    })),
                )?;
                ctx.eval::<(), _>(format!(
                    "(() => {{ const c = __call; globalThis[{name}] = async (...args) => {{\
                       const r = JSON.parse(await c(JSON.stringify(args)));\
                       if ('error' in r) throw new Error(r.error);\
                       return r.ok;\
                     }}; delete globalThis.__call; }})()",
                    name = serde_json::to_string(name).unwrap_or_default()
                ))?;
            }
            ctx.eval::<(), _>(
                "(() => { const log = __log; const show = (a) => a.map(x => \
                   typeof x === 'string' ? x : JSON.stringify(x)).join(' ');\
                   globalThis.console = { log: (...a) => log(show(a)), info: (...a) => log(show(a)),\
                   warn: (...a) => log(show(a)), error: (...a) => log(show(a)) };\
                   delete globalThis.__log; })()",
            )?;
            Ok(())
        })();
        if let Err(e) = setup.catch(&ctx) {
            return Err(describe(e));
        }

        // The model's function, called; or a bare expression's value.
        let value: Value = match ctx.eval::<Value, _>(format!("(\n{code}\n)")).catch(&ctx) {
            Ok(v) => v,
            Err(e) => return Err(describe(e)),
        };
        let value = match value.as_function() {
            Some(f) => match f.call::<(), Value>(()).catch(&ctx) {
                Ok(v) => v,
                Err(e) => return Err(describe(e)),
            },
            None => value,
        };
        let value = match value.clone().into_promise() {
            Some(p) => match Promise::into_future::<Value>(p).await.catch(&ctx) {
                Ok(v) => v,
                Err(e) => return Err(describe(e)),
            },
            None => value,
        };
        match ctx.json_stringify(value).catch(&ctx) {
            Ok(Some(s)) => s.to_string().map_err(|e| e.to_string()),
            Ok(None) => Ok("null".to_owned()),
            Err(e) => Err(describe(e)),
        }
    });

    let result = match tokio::time::timeout(limits.wall, work).await {
        Ok(r) => r,
        Err(_) => Err(stop_message(STOP_WALL, &limits)),
    };
    // An interrupt surfaces as an opaque exception: say which limit it was.
    let result = match (result, stopped.load(Ordering::Relaxed)) {
        (Err(_), why) if why != STOP_NONE => Err(stop_message(why, &limits)),
        (Ok(text), _) if text.chars().count() > max_output => Ok(format!(
            "{}\n[result truncated at {max_output} characters: return less]",
            text.chars().take(max_output).collect::<String>()
        )),
        (r, _) => r,
    };
    let logs = logs.lock().map(|l| l.clone()).unwrap_or_default();
    Outcome {
        result,
        logs,
        calls: calls.load(Ordering::Relaxed),
    }
}

/// A tool call that stops when the script stops waiting for it (its deadline
/// passed), rather than running on unseen.
struct Abort<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for Abort<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn stop_message(why: usize, limits: &Limits) -> String {
    match why {
        STOP_BUSY => format!(
            "stopped: the script ran JavaScript for over {}s (an endless loop?)",
            limits.busy.as_secs()
        ),
        _ => format!(
            "stopped: the script took over {}s in all",
            limits.wall.as_secs()
        ),
    }
}

/// A thrown error as the model should read it: message, then stack.
fn describe(e: CaughtError<'_>) -> String {
    match e {
        CaughtError::Exception(ex) => {
            let message = ex.message().unwrap_or_default();
            match ex.stack().filter(|s| !s.trim().is_empty()) {
                Some(stack) => format!("{message}\n{}", stack.trim_end()),
                None => message,
            }
        }
        CaughtError::Value(v) => v
            .as_string()
            .and_then(|s| s.to_string().ok())
            .unwrap_or_else(|| format!("thrown: {v:?}")),
        CaughtError::Error(e) => e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    // Tests may unwrap and expect: a panic is the failure report.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// Echoes its arguments, or fails when asked to.
    struct Echo;

    #[async_trait::async_trait]
    impl Host for Echo {
        async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, String> {
            if args[0] == "fail" {
                return Err("no such tool".to_owned());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
            Ok(serde_json::json!({ "echo": args }))
        }
    }

    fn echo() -> Option<(String, Arc<dyn Host>)> {
        Some(("call".to_owned(), Arc::new(Echo) as Arc<dyn Host>))
    }

    async fn go(code: &str) -> Outcome {
        run(code, Vec::new(), echo(), Limits::default()).await
    }

    #[tokio::test]
    async fn a_script_searches_its_globals_and_returns_only_what_it_picks() {
        let catalog = serde_json::json!([
            {"name": "decompile", "description": "pseudocode of a function"},
            {"name": "strings", "description": "printable strings"},
        ]);
        let out = run(
            "async () => catalog.filter(t => t.description.includes('function')).map(t => t.name)",
            vec![("catalog".to_owned(), catalog)],
            None,
            Limits::default(),
        )
        .await;
        assert_eq!(out.result, Ok(r#"["decompile"]"#.to_owned()));
    }

    #[tokio::test]
    async fn calls_chain_and_their_answers_are_objects() {
        let out = go("async () => {
                const a = await call('first', {x: 1});
                const b = await call('second', a.echo[1]);
                console.log('done', {n: 2});
                return [a.echo[0], b.echo[1].x];
            }")
        .await;
        assert_eq!(out.result, Ok(r#"["first",1]"#.to_owned()));
        assert_eq!(out.calls, 2);
        assert_eq!(out.logs, [r#"done {"n":2}"#]);
    }

    #[tokio::test]
    async fn a_host_error_is_thrown_and_can_be_caught() {
        let out = go("async () => { try { await call('fail') } catch (e) { return 'caught: ' + e.message } }").await;
        assert_eq!(out.result, Ok(r#""caught: no such tool""#.to_owned()));
        let out = go("async () => await call('fail')").await;
        assert!(out.result.unwrap_err().contains("no such tool"));
    }

    #[tokio::test]
    async fn the_sandbox_has_no_way_out() {
        let out = go(
            "() => ['fetch','require','process','setTimeout','XMLHttpRequest','Deno','Bun','__call','__log','os','std']
                .filter(n => typeof globalThis[n] !== 'undefined')",
        )
        .await;
        assert_eq!(out.result, Ok("[]".to_owned()), "nothing but the language");
        let out = go("async () => { const m = await import('fs'); return 1 }").await;
        assert!(out.result.is_err(), "no modules: {out:?}");
    }

    #[tokio::test]
    async fn an_endless_loop_is_stopped() {
        let limits = Limits {
            busy: Duration::from_millis(300),
            ..Limits::default()
        };
        let started = Instant::now();
        let out = run("() => { while (true) {} }", Vec::new(), None, limits).await;
        assert!(out.result.unwrap_err().contains("endless loop"));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn waiting_on_calls_does_not_count_as_running() {
        struct Slow;
        #[async_trait::async_trait]
        impl Host for Slow {
            async fn call(&self, _: serde_json::Value) -> Result<serde_json::Value, String> {
                tokio::time::sleep(Duration::from_millis(400)).await;
                Ok(serde_json::json!(1))
            }
        }
        let limits = Limits {
            busy: Duration::from_millis(200),
            ..Limits::default()
        };
        let out = run(
            "async () => (await call()) + (await call())",
            Vec::new(),
            Some(("call".to_owned(), Arc::new(Slow) as Arc<dyn Host>)),
            limits,
        )
        .await;
        assert_eq!(out.result, Ok("2".to_owned()));
    }

    #[tokio::test]
    async fn the_whole_script_has_a_deadline_and_its_call_is_cancelled() {
        /// Never answers; counts how many of its calls are still running.
        struct Never(Arc<AtomicUsize>);
        struct Running(Arc<AtomicUsize>);
        impl Drop for Running {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::Relaxed);
            }
        }
        #[async_trait::async_trait]
        impl Host for Never {
            async fn call(&self, _: serde_json::Value) -> Result<serde_json::Value, String> {
                self.0.fetch_add(1, Ordering::Relaxed);
                let _running = Running(self.0.clone());
                std::future::pending().await
            }
        }
        let running = Arc::new(AtomicUsize::new(0));
        let limits = Limits {
            wall: Duration::from_millis(300),
            ..Limits::default()
        };
        let out = run(
            "async () => await call()",
            Vec::new(),
            Some((
                "call".to_owned(),
                Arc::new(Never(running.clone())) as Arc<dyn Host>,
            )),
            limits,
        )
        .await;
        assert!(out.result.unwrap_err().contains("took over"));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(running.load(Ordering::Relaxed), 0, "the call was cancelled");
    }

    #[tokio::test]
    async fn memory_and_calls_are_capped() {
        let limits = Limits {
            memory_bytes: 16 * 1024 * 1024,
            ..Limits::default()
        };
        let out = run(
            "() => { const a = []; while (true) a.push('x'.repeat(1000)); }",
            Vec::new(),
            None,
            limits,
        )
        .await;
        assert!(out.result.is_err(), "{out:?}");

        let limits = Limits {
            max_calls: 3,
            ..Limits::default()
        };
        let out = run(
            "async () => { for (let i = 0; i < 5; i++) await call(i) }",
            Vec::new(),
            echo(),
            limits,
        )
        .await;
        assert!(out.result.unwrap_err().contains("at most 3 calls"));
        assert_eq!(out.calls, 4, "the fourth was refused");
    }

    #[tokio::test]
    async fn mistakes_come_back_readable() {
        let out = go("async () => { const x = undefinedThing.y; }").await;
        let e = out.result.unwrap_err();
        assert!(e.contains("undefinedThing"), "{e}");
        let out = go("async () => {").await;
        assert!(out.result.is_err(), "a syntax error is reported");
        let out = go("async () => {}").await;
        assert_eq!(out.result, Ok("null".to_owned()), "undefined is null");
        let out = run(
            "() => 'x'.repeat(100)",
            Vec::new(),
            None,
            Limits {
                max_output: 10,
                ..Limits::default()
            },
        )
        .await;
        assert!(out.result.unwrap().contains("truncated"));
    }
}
