//! A coordinated turn's workers, running in the background while the lead
//! keeps going: `dispatch_agent` starts one and returns at once, and
//! `wait_for_team` hears from them. Workers and lead share the turn's task:
//! the lead's loop and [`Team::drive`] are joined, so nothing outlives the turn.

use futures_util::StreamExt as _;
use futures_util::stream::FuturesUnordered;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// How long one `wait_for_team` waits when nothing needs the lead: long
/// enough to batch routine updates, short enough to steer a worker in time.
pub(crate) const TEAM_WAIT: Duration = Duration::from_secs(60);

/// How often a waiting lead checks for a message addressed to it.
const POLL: Duration = Duration::from_secs(2);

/// A worker's run, ending in its report.
pub(crate) type Work<'a> = Pin<Box<dyn Future<Output = Report> + Send + 'a>>;

#[derive(Debug)]
pub(crate) struct Report {
    pub dispatch: String,
    pub agent: String,
    pub text: String,
    pub is_error: bool,
}

pub(crate) struct Team<'a> {
    queue: Mutex<Vec<Work<'a>>>,
    added: Notify,
    closed: AtomicBool,
    /// Running workers, as (dispatch id, agent name), in start order.
    running: Mutex<Vec<(String, String)>>,
    reports: Mutex<Vec<Report>>,
    arrived: Notify,
    /// How far the lead has read the team's messages.
    pub cursor: Mutex<usize>,
    /// Stops the workers: the turn's cancel, or the turn ending in error.
    pub cancel: CancellationToken,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl<'a> Team<'a> {
    pub fn new(cancel: &CancellationToken) -> Self {
        Self {
            queue: Mutex::default(),
            added: Notify::new(),
            closed: AtomicBool::new(false),
            running: Mutex::default(),
            reports: Mutex::default(),
            arrived: Notify::new(),
            cursor: Mutex::new(0),
            cancel: cancel.child_token(),
        }
    }

    pub fn running(&self) -> Vec<(String, String)> {
        lock(&self.running).clone()
    }

    pub fn start(&self, dispatch: String, agent: String, work: Work<'a>) {
        lock(&self.running).push((dispatch, agent));
        lock(&self.queue).push(work);
        self.added.notify_one();
    }

    pub fn take_reports(&self) -> Vec<Report> {
        std::mem::take(&mut *lock(&self.reports))
    }

    /// No more workers will start: [`Team::drive`] ends once these finish.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.added.notify_one();
    }

    /// Run the workers as they start, until closed and all have reported.
    pub async fn drive(&self) {
        let mut set = FuturesUnordered::new();
        loop {
            set.extend(lock(&self.queue).drain(..));
            if set.is_empty() {
                if self.closed.load(Ordering::SeqCst) {
                    return;
                }
                self.added.notified().await;
                continue;
            }
            tokio::select! {
                Some(report) = set.next() => {
                    lock(&self.running).retain(|(d, _)| *d != report.dispatch);
                    lock(&self.reports).push(report);
                    self.arrived.notify_one();
                }
                () = self.added.notified() => {}
            }
        }
    }

    /// Wait until there is a report, `wanted()` (a message for the lead), no
    /// one is working, the turn is cancelled, or `max` has passed.
    pub async fn settle(&self, max: Duration, wanted: impl Fn() -> bool) {
        let deadline = tokio::time::Instant::now() + max;
        loop {
            if !lock(&self.reports).is_empty()
                || wanted()
                || lock(&self.running).is_empty()
                || self.cancel.is_cancelled()
            {
                return;
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return;
            }
            tokio::select! {
                () = self.arrived.notified() => {}
                () = tokio::time::sleep((deadline - now).min(POLL)) => {}
                () = self.cancel.cancelled() => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn report(id: &str) -> Report {
        Report {
            dispatch: id.to_owned(),
            agent: "worker".to_owned(),
            text: format!("{id} done"),
            is_error: false,
        }
    }

    #[tokio::test]
    async fn workers_run_while_the_lead_goes_on_and_drive_ends_once_closed() {
        let team = Team::new(&CancellationToken::new());
        let lead = async {
            team.start("a".into(), "worker".into(), Box::pin(async { report("a") }));
            team.settle(Duration::from_secs(5), || false).await;
            let first = team.take_reports();
            team.start(
                "b".into(),
                "worker".into(),
                Box::pin(async {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    report("b")
                }),
            );
            assert_eq!(team.running().len(), 1);
            team.settle(Duration::from_secs(5), || false).await;
            let second = team.take_reports();
            team.close();
            (first, second)
        };
        let ((first, second), ()) = tokio::join!(lead, team.drive());
        assert_eq!(first[0].text, "a done");
        assert_eq!(second[0].text, "b done");
        assert!(team.running().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn settle_gives_up_after_its_wait_when_nothing_happens() {
        let team = Team::new(&CancellationToken::new());
        team.start(
            "a".into(),
            "worker".into(),
            Box::pin(std::future::pending()),
        );
        let start = tokio::time::Instant::now();
        // Not driven: the worker never finishes.
        team.settle(Duration::from_secs(60), || false).await;
        assert!(start.elapsed() >= Duration::from_secs(60));
        assert_eq!(team.running().len(), 1);
    }
}
