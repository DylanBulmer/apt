//! The lifecycle policy, written once for every console.
//!
//! Each function here is the whole body of a plugin's hook: the plugin
//! connects, hands over a [`Console`], and this decides what happens. None of
//! it returns an error to core — see the note on each — and none of it knows
//! which transport it is driving.

use std::time::Duration;

use crate::Console;
use crate::countdown::{self, MARKS, PlayerCount};

/// How long the JVM is given to flush chunks and exit before systemd's
/// `TimeoutStopSec` runs out. Part of the arithmetic asserted in
/// [`crate::countdown`]'s tests.
pub const CHUNK_FLUSH_GRACE: Duration = Duration::from_secs(10);

/// How long a `save-all` is given to reach disk before an archive reads it.
const SAVE_SETTLE: Duration = Duration::from_secs(3);

/// `pre-stop` — warn whoever is online, then ask the server to stop.
///
/// NEVER FAILS. A shutdown that aborted because a warning could not be
/// delivered would leave the operator with a server they cannot stop, and core
/// refuses to let this event be declared fatal for the same reason.
pub fn pre_stop<C: Console>(console: &mut C, log: &dyn Fn(&str)) {
    log("Stop requested; asking the server who is online.");

    match console.player_count() {
        PlayerCount::Online(0) => {
            log("No players online — skipping the countdown and stopping immediately.");
        }
        PlayerCount::Online(n) => {
            log(&format!(
                "{n} player(s) online — triggering the 5-minute countdown."
            ));
            countdown_to_stop(console, log);
        }
        // Logged differently from a counted zero on purpose: the journal must
        // distinguish "we counted" from "we could not count", because the
        // latter also says the console itself is in trouble.
        PlayerCount::Unknown => {
            log(
                "Player count unavailable — assuming players are online; triggering the 5-minute countdown.",
            );
            countdown_to_stop(console, log);
        }
    }

    log("Asking the server to stop.");
    if let Err(e) = console.stop() {
        log(&format!("WARNING: the stop request failed: {e}"));
    }

    log(&format!(
        "Waiting {}s for the server to flush chunks and exit.",
        CHUNK_FLUSH_GRACE.as_secs()
    ));
    console.wait(CHUNK_FLUSH_GRACE);
}

fn countdown_to_stop<C: Console>(console: &mut C, log: &dyn Fn(&str)) {
    for step in countdown::schedule(&MARKS) {
        // A failed announcement is reported and then ignored: players silently
        // not being warned is exactly what an operator needs to know about,
        // and the shutdown proceeds on schedule either way.
        match console.say(&step.message) {
            Ok(()) => log(&format!("Announced to players: {}", step.message)),
            Err(e) => log(&format!(
                "WARNING: could not announce '{}': {e}",
                step.message
            )),
        }
        if !step.wait.is_zero() {
            // Logged so a long quiet stretch reads as "waiting on purpose"
            // rather than "wedged".
            log(&format!("Next warning in {}s.", step.wait.as_secs()));
            console.wait(step.wait);
        }
    }
}

/// `pre-backup` — flush the world and hold it still for the archive.
///
/// Best-effort throughout: a backup of an unflushed world beats no backup, so
/// nothing here is allowed to abort the operation.
pub fn pre_backup<C: Console>(console: &mut C, log: &dyn Fn(&str)) {
    let _ = console.say("[mc] Backup starting — brief lag possible");

    if let Err(e) = console.set_autosave(false) {
        log(&format!("WARNING: could not pause autosave: {e}"));
    }
    if let Err(e) = console.save_now() {
        log(&format!(
            "WARNING: could not flush the world ({e}); the archive may be of an unflushed world."
        ));
    }
    // A moment for the write to reach disk before the archive starts reading.
    console.wait(SAVE_SETTLE);
}

/// `post-backup` — turn saving back on.
///
/// RUNS WHETHER OR NOT THE BACKUP SUCCEEDED. A live server left with saving
/// disabled loses everything since the last flush the moment it stops, which
/// is worse than the failed backup that got here.
pub fn post_backup<C: Console>(console: &mut C, log: &dyn Fn(&str)) {
    if let Err(e) = console.set_autosave(true) {
        // The one failure in this file worth shouting about: the server is
        // still running, and it is now not saving.
        log(&format!(
            "WARNING: COULD NOT RE-ENABLE AUTOSAVE ({e}) — the server is running without saving."
        ));
    }
    let _ = console.say("[mc] Backup complete");
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_common::error::Result;
    use std::cell::RefCell;

    /// A console that records what it was asked to do, including how long it
    /// was asked to wait — so the schedule's pacing is asserted as data
    /// rather than endured.
    #[derive(Default)]
    struct FakeConsole {
        count: Option<PlayerCount>,
        calls: RefCell<Vec<String>>,
        fail_say: bool,
        fail_autosave: bool,
    }

    impl FakeConsole {
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl Console for FakeConsole {
        fn say(&mut self, message: &str) -> Result<()> {
            self.calls.borrow_mut().push(format!("say:{message}"));
            if self.fail_say {
                return Err(mc_common::error::Error::other("no".to_string()));
            }
            Ok(())
        }
        fn player_count(&mut self) -> PlayerCount {
            self.calls.borrow_mut().push("count".to_string());
            self.count.unwrap_or(PlayerCount::Unknown)
        }
        fn save_now(&mut self) -> Result<()> {
            self.calls.borrow_mut().push("save".to_string());
            Ok(())
        }
        fn set_autosave(&mut self, enabled: bool) -> Result<()> {
            self.calls.borrow_mut().push(format!("autosave:{enabled}"));
            if self.fail_autosave {
                return Err(mc_common::error::Error::other("no".to_string()));
            }
            Ok(())
        }
        fn stop(&mut self) -> Result<()> {
            self.calls.borrow_mut().push("stop".to_string());
            Ok(())
        }
        fn wait(&mut self, duration: std::time::Duration) {
            // Recorded, never taken: the schedule is five minutes long.
            self.calls
                .borrow_mut()
                .push(format!("wait:{}", duration.as_secs()));
        }
    }

    fn silent() -> impl Fn(&str) {
        |_| {}
    }

    #[test]
    fn a_provably_empty_server_is_stopped_without_a_countdown() {
        // Five minutes of warnings nobody is there to read is five minutes of
        // downtime for nothing.
        let mut console = FakeConsole {
            count: Some(PlayerCount::Online(0)),
            ..Default::default()
        };
        pre_stop(&mut console, &silent());

        let calls = console.calls();
        assert_eq!(calls, vec!["count", "stop", "wait:10"], "no announcements");
    }

    #[test]
    fn an_unknown_count_is_warned_exactly_like_a_populated_server() {
        // The conservative branch: not being able to count must never be
        // treated as "empty", or a full server is dropped without warning.
        let mut console = FakeConsole {
            count: Some(PlayerCount::Unknown),
            ..Default::default()
        };
        pre_stop(&mut console, &silent());

        let announcements: Vec<String> = console
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("say:"))
            .collect();
        assert_eq!(announcements.len(), MARKS.len());
        assert!(announcements[0].contains("5 minutes"));
        assert!(announcements[2].contains("1 minute"));
    }

    #[test]
    fn the_server_is_still_stopped_when_every_announcement_fails() {
        // Players not being warned is a problem to report, not a reason to
        // leave the operator with a server that will not stop.
        let mut console = FakeConsole {
            count: Some(PlayerCount::Online(3)),
            fail_say: true,
            ..Default::default()
        };
        pre_stop(&mut console, &silent());
        assert!(console.calls().contains(&"stop".to_string()));
    }

    #[test]
    fn a_backup_pauses_saving_then_flushes_in_that_order() {
        // Flushing first and pausing second would let the server write again
        // between the two, which is the inconsistent archive this exists to
        // prevent.
        let mut console = FakeConsole::default();
        pre_backup(&mut console, &silent());

        let calls: Vec<String> = console
            .calls()
            .into_iter()
            .filter(|c| !c.starts_with("say:"))
            .collect();
        // The settle wait belongs after the flush, not before: it exists to
        // let the write reach disk before the archive starts reading.
        assert_eq!(calls, vec!["autosave:false", "save", "wait:3"]);
    }

    #[test]
    fn saving_is_restored_even_when_pausing_it_failed() {
        let mut console = FakeConsole {
            fail_autosave: true,
            ..Default::default()
        };
        post_backup(&mut console, &silent());
        assert!(console.calls().contains(&"autosave:true".to_string()));
    }

    #[test]
    fn a_failure_to_restore_saving_is_reported_loudly() {
        // The one case where a silent best-effort would be wrong: the server
        // is up and no longer writing to disk.
        let logged = RefCell::new(Vec::new());
        let log = |m: &str| logged.borrow_mut().push(m.to_string());

        let mut console = FakeConsole {
            fail_autosave: true,
            ..Default::default()
        };
        post_backup(&mut console, &log);

        assert!(
            logged.borrow().iter().any(|m| m.contains("WARNING")),
            "{:?}",
            logged.borrow()
        );
    }
}
