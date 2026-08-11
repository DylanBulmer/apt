//! The shutdown countdown.
//!
//! Two outcomes only. Either the server is provably empty and it stops at once,
//! or somebody might be affected and they get the full warning:
//!
//! | players | warning | announced at    |
//! |---------|---------|-----------------|
//! | 0       | none    | (stop at once)  |
//! | 1+      | 5 min   | 5 min, 3, 1     |
//! | unknown | 5 min   | 5 min, 3, 1     |
//!
//! The last two rows behave identically but are LOGGED differently on purpose:
//! the journal must distinguish "we counted N players" from "we could not count
//! at all", because the latter also indicates a console problem.
//!
//! IF THESE MARKS CHANGE, RECOMPUTE `TimeoutStopSec` in `minecraft.service`.
//! That value is derived arithmetic over the worst-case path through here, and
//! overrunning it means the JVM is SIGKILLed mid-chunk-flush — the corruption
//! this countdown exists to prevent.

use std::time::Duration;

/// Seconds-remaining marks at which players are warned.
pub const MARKS: [u32; 3] = [300, 180, 60];

/// How many players are online.
///
/// `Unknown` is NOT zero, and the distinction decides how long a shutdown
/// takes. Every failure mode — no console installed, connection refused, auth
/// failure, timeout, a reply in a wording nobody recognises — resolves here,
/// and the countdown treats it exactly like "players are online". An
/// unnecessary wait on an empty server is far cheaper than cutting off real
/// players without warning.
///
/// Lives beside the schedule rather than beside either console's transport:
/// how the count was obtained is the console's business, and what to do about
/// it is policy every console must apply identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerCount {
    Online(u32),
    Unknown,
}

impl PlayerCount {
    /// True when the server is PROVABLY empty. Anything else warns.
    pub fn provably_empty(&self) -> bool {
        matches!(self, PlayerCount::Online(0))
    }
}

/// One step of a countdown: what to announce, and how long to wait afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub message: String,
    pub wait: Duration,
}

/// Expand the marks into announcements and gaps.
///
/// A pure function so the schedule can be asserted without waiting for it, and
/// so the total can be checked against the unit's timeout in a test rather than
/// by hand.
pub fn schedule(marks: &[u32]) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut previous: Option<u32> = None;

    for &mark in marks {
        if let Some(prev) = previous {
            // The gap belongs to the PREVIOUS step: announce, then wait until
            // the next mark.
            if let Some(last) = steps.last_mut() {
                let step: &mut Step = last;
                step.wait = Duration::from_secs(u64::from(prev.saturating_sub(mark)));
            }
        }
        steps.push(Step {
            message: announcement(mark),
            wait: Duration::ZERO,
        });
        previous = Some(mark);
    }

    // After the final announcement, wait out the last mark itself.
    if let (Some(last_mark), Some(last)) = (previous, steps.last_mut()) {
        let step: &mut Step = last;
        step.wait = Duration::from_secs(u64::from(last_mark));
    }
    steps
}

fn announcement(seconds_remaining: u32) -> String {
    if seconds_remaining >= 60 && seconds_remaining.is_multiple_of(60) {
        let minutes = seconds_remaining / 60;
        if minutes == 1 {
            "[Server] Shutting down in 1 minute.".to_string()
        } else {
            format!("[Server] Shutting down in {minutes} minutes.")
        }
    } else {
        format!("[Server] Shutting down in {seconds_remaining} seconds.")
    }
}

/// Total wall-clock time the schedule spends waiting.
pub fn total_wait(steps: &[Step]) -> Duration {
    steps.iter().map(|s| s.wait).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announces_at_five_three_and_one_minutes() {
        let steps = schedule(&MARKS);
        let messages: Vec<&str> = steps.iter().map(|s| s.message.as_str()).collect();
        assert_eq!(
            messages,
            vec![
                "[Server] Shutting down in 5 minutes.",
                "[Server] Shutting down in 3 minutes.",
                "[Server] Shutting down in 1 minute.",
            ]
        );
    }

    #[test]
    fn the_gaps_add_up_to_the_first_mark() {
        // 5 minutes of warning means five minutes elapse, not more.
        let steps = schedule(&MARKS);
        assert_eq!(total_wait(&steps), Duration::from_secs(300));
        assert_eq!(steps[0].wait, Duration::from_secs(120));
        assert_eq!(steps[1].wait, Duration::from_secs(120));
        assert_eq!(steps[2].wait, Duration::from_secs(60));
    }

    #[test]
    fn the_schedule_fits_inside_the_units_stop_timeout() {
        // TimeoutStopSec=380 is derived from this path:
        //   console election     3 s   (plugin::PROBE_DEADLINE, see below)
        //   list query          15 s   (CMD_DEADLINE)
        //   countdown          300 s
        //   3 announcements     15 s   (3 x say budget, worst case)
        //   stop command        15 s
        //   chunk-flush sleep   10 s
        //   ------------------------
        //   worst case         358 s   + 22 s buffer = 380 s
        //
        // ONE probe deadline, not one per installed console: the election
        // stops at the first console that answers, and if none answers there
        // is no elected console and therefore no countdown to pay for. The
        // expensive path and the slow-probe path cannot both happen.
        //
        // If this fails, the countdown was re-tiered — or a probe deadline
        // moved — without recomputing the unit, and overrunning
        // TimeoutStopSec means a SIGKILL through the JVM's chunk flush.
        //
        // READ FROM THE UNIT, not restated here. A copy of the number would
        // let the two drift in exactly the direction this test exists to
        // catch: the arithmetic changes, the constant is updated, and the
        // shipped unit keeps the old value.
        let timeout_stop_sec = unit_timeout_stop_sec();
        const SAFETY_BUFFER: u64 = 22;

        let worst_case = worst_case_stop_seconds();
        assert_eq!(worst_case, 358);
        assert!(
            worst_case + SAFETY_BUFFER <= timeout_stop_sec,
            "countdown worst case {worst_case}s + {SAFETY_BUFFER}s buffer exceeds \
             TimeoutStopSec={timeout_stop_sec}s in minecraft.service"
        );
    }

    /// The worst-case stop path in seconds, itemised in the test above.
    ///
    /// Computed once so everything derived from this budget moves with it. A
    /// second copy of the arithmetic would drift in exactly the direction these
    /// tests exist to catch.
    fn worst_case_stop_seconds() -> u64 {
        // list query, 3 announcements, stop command, chunk-flush sleep.
        const NON_COUNTDOWN_BUDGET: u64 = 15 + 15 + 15 + 10;
        total_wait(&schedule(&MARKS)).as_secs()
            + NON_COUNTDOWN_BUDGET
            + mc_common::plugin::PROBE_DEADLINE.as_secs()
    }

    #[test]
    fn the_hook_deadline_clears_the_countdown_it_has_to_contain() {
        // The elected console's `pre-stop` hook IS most of the stop path: core
        // spawns it and kills it at `plugin::HOOK_DEADLINE`. The election
        // happens in core BEFORE the hook, so what the hook itself may
        // legitimately spend is the worst case minus that one probe.
        //
        // A deadline below that figure kills a HEALTHY countdown partway
        // through — the SIGKILL through the JVM's chunk flush that the
        // countdown exists to prevent — and it does so reported as a hung
        // plugin, which sends the operator looking at the console rather than
        // at a budget that no longer adds up. Re-tiering MARKS is the way this
        // happens: the derivation is prose in mc-plugins(5), the
        // plugin-development skill and the README, and none of those fail.
        //
        // Both sides come from the constants themselves. A test that restated
        // 360 or 355 would go stale exactly as silently as the prose.
        let hook_deadline = mc_common::plugin::HOOK_DEADLINE.as_secs();
        let election = mc_common::plugin::PROBE_DEADLINE.as_secs();
        let inside_the_hook = worst_case_stop_seconds() - election;

        assert!(
            inside_the_hook <= hook_deadline,
            "a healthy pre-stop spends {inside_the_hook}s, past the {hook_deadline}s \
             plugin::HOOK_DEADLINE — core would kill a countdown that was working"
        );

        // And the bound the deadline imposes is still one the unit can absorb:
        // after the kill, `mc shutdown` has to report it and exit inside
        // TimeoutStopSec, or systemd SIGKILLs the JVM anyway and the bounded
        // overrun buys nothing.
        let timeout_stop_sec = unit_timeout_stop_sec();
        assert!(
            election + hook_deadline <= timeout_stop_sec,
            "an election plus a hook killed at its deadline is {}s, past \
             TimeoutStopSec={timeout_stop_sec}s",
            election + hook_deadline
        );
    }

    /// `TimeoutStopSec=` as the shipped unit actually declares it.
    fn unit_timeout_stop_sec() -> u64 {
        let unit = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packages/mc-server/lib/systemd/system/minecraft.service");
        let text =
            std::fs::read_to_string(&unit).unwrap_or_else(|e| panic!("{}: {e}", unit.display()));

        text.lines()
            .find_map(|line| line.strip_prefix("TimeoutStopSec="))
            .and_then(|value| value.trim().trim_end_matches('s').parse().ok())
            .expect("minecraft.service declares TimeoutStopSec in seconds")
    }

    #[test]
    fn sub_minute_marks_are_announced_in_seconds() {
        // Unreachable with the current tiers, and kept working on purpose: the
        // tier table is a knob an operator is expected to turn, and a general
        // helper makes re-tiering a one-line change. Do not delete this branch
        // without also fixing MARKS.
        let steps = schedule(&[90, 30, 10]);
        let messages: Vec<&str> = steps.iter().map(|s| s.message.as_str()).collect();
        assert_eq!(
            messages,
            vec![
                "[Server] Shutting down in 90 seconds.",
                "[Server] Shutting down in 30 seconds.",
                "[Server] Shutting down in 10 seconds.",
            ]
        );
        assert_eq!(total_wait(&steps), Duration::from_secs(90));
    }

    #[test]
    fn an_empty_tier_table_produces_no_wait() {
        assert!(schedule(&[]).is_empty());
        assert_eq!(total_wait(&schedule(&[])), Duration::ZERO);
    }
}
