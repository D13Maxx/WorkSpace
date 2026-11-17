//! Keeps the PC from idling to sleep while agents work, so a turn left running
//! finishes. Closing the lid still sleeps unless the user's power settings say
//! otherwise: Windows treats that as the user asking for it.

use std::sync::Mutex;

/// Turns running, and the power request held while any are.
static HELD: Mutex<(usize, Option<keepawake::KeepAwake>)> = Mutex::new((0, None));

/// Held for as long as a turn runs.
pub struct Awake;

pub fn hold() -> Awake {
    if let Ok(mut held) = HELD.lock() {
        held.0 += 1;
        if held.0 == 1 {
            // Shown by `powercfg /requests`.
            held.1 = keepawake::Builder::default()
                .idle(true)
                // Modern Standby laptops enter standby when the idle timer turns
                // the screen off, whatever system-required says (seen in the
                // event log mid-turn): only a display request stops that.
                .display(true)
                .reason("Agents are working")
                .app_name("Workspace")
                .app_reverse_domain("dev.debargha.friday")
                .create()
                .inspect_err(|e| tracing::warn!(error = %e, "could not keep the PC awake; it may sleep mid-turn"))
                .ok();
        }
    }
    Awake
}

impl Drop for Awake {
    fn drop(&mut self) {
        if let Ok(mut held) = HELD.lock() {
            held.0 = held.0.saturating_sub(1);
            if held.0 == 0 {
                held.1 = None; // dropping it lets the PC sleep again
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_request_is_held_while_any_turn_runs() {
        let a = hold();
        let b = hold();
        // A machine with no power service to ask (a CI box) grants nothing.
        let granted = HELD.lock().unwrap().1.is_some();
        assert_eq!(HELD.lock().unwrap().0, 2);
        drop(a);
        assert_eq!(HELD.lock().unwrap().0, 1);
        assert_eq!(HELD.lock().unwrap().1.is_some(), granted, "b still runs");
        drop(b);
        assert_eq!(HELD.lock().unwrap().0, 0);
        assert!(HELD.lock().unwrap().1.is_none());
    }
}
