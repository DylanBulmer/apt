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
        // TimeoutStopSec=375 is derived from this path:
        //   list query          15 s   (CMD_DEADLINE)
        //   countdown          300 s
        //   3 announcements     15 s   (3 x say budget, worst case)
        //   stop command        15 s
        //   chunk-flush sleep   10 s
        //   ------------------------
        //   worst case         355 s   + 20 s buffer = 375 s
        //
        // If this fails, the countdown was re-tiered without recomputing the
        // unit — and overrunning TimeoutStopSec means a SIGKILL through the
        // JVM's chunk flush.
        const TIMEOUT_STOP_SEC: u64 = 375;
        const NON_COUNTDOWN_BUDGET: u64 = 15 + 15 + 15 + 10;
        const SAFETY_BUFFER: u64 = 20;

        let worst_case = total_wait(&schedule(&MARKS)).as_secs() + NON_COUNTDOWN_BUDGET;
        assert_eq!(worst_case, 355);
        assert!(
            worst_case + SAFETY_BUFFER <= TIMEOUT_STOP_SEC,
            "countdown worst case {worst_case}s + buffer exceeds TimeoutStopSec={TIMEOUT_STOP_SEC}s"
        );
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
