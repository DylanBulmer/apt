//! Driving the systemd unit, behind a trait.
//!
//! Behind a trait because the interesting behaviour here is *timing*, and
//! timing is what a container cannot reproduce. `Type=simple` reports success
//! the moment the process is forked, so `systemctl start` returning 0 says
//! nothing about whether the server survived the next half second — and the
//! code that copes with that is exactly what needs testing. A scripted fake can
//! produce "start succeeded, then the unit failed" on demand; neither a real
//! systemd nor the shell stub can.

use std::time::Duration;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitState {
    Active,
    Failed,
    Inactive,
    /// systemd is not the running init — a container, a chroot, a build host.
    Absent,
}

pub trait ServiceManager: Send + Sync {
    fn state(&self, unit: &str) -> UnitState;
    fn start(&self, unit: &str) -> Result<()>;
    fn stop(&self, unit: &str) -> Result<()>;
    fn enable(&self, unit: &str) -> Result<()>;
    fn disable(&self, unit: &str) -> Result<()>;
    fn daemon_reload(&self) -> Result<()>;
    /// Last few journal lines for a unit, for reporting why a start failed.
    fn recent_log(&self, unit: &str, lines: u32) -> Option<String>;
    /// Sleep. On the trait so a fake can make the start poll and the shutdown
    /// countdown run instantly instead of in real time.
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }

    fn is_active(&self, unit: &str) -> bool {
        self.state(unit) == UnitState::Active
    }

    fn is_failed(&self, unit: &str) -> bool {
        self.state(unit) == UnitState::Failed
    }
}

/// The real implementation: shells out to `systemctl`.
pub struct Systemctl {
    /// False when `/run/systemd/system` is absent, in which case every call is
    /// a no-op rather than a guaranteed error. `systemctl` the binary exists in
    /// plenty of places systemd is not running.
    available: bool,
}

impl Systemctl {
    pub fn new(available: bool) -> Self {
        Self { available }
    }

    fn run(&self, args: &[&str]) -> Result<()> {
        if !self.available {
            return Ok(());
        }
        let status = std::process::Command::new("systemctl")
            .args(args)
            .status()
            .map_err(|e| Error::other(format!("systemctl {}: {e}", args.join(" "))))?;
        if status.success() {
            Ok(())
        } else {
            Err(Error::other(format!(
                "systemctl {} failed with {status}",
                args.join(" ")
            )))
        }
    }
}

impl ServiceManager for Systemctl {
    fn state(&self, unit: &str) -> UnitState {
        if !self.available {
            return UnitState::Absent;
        }
        let quiet = |args: &[&str]| {
            std::process::Command::new("systemctl")
                .args(args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        if quiet(&["is-active", "--quiet", unit]) {
            UnitState::Active
        } else if quiet(&["is-failed", "--quiet", unit]) {
            UnitState::Failed
        } else {
            UnitState::Inactive
        }
    }

    fn start(&self, unit: &str) -> Result<()> {
        self.run(&["start", unit])
    }

    fn stop(&self, unit: &str) -> Result<()> {
        self.run(&["stop", unit])
    }

    fn enable(&self, unit: &str) -> Result<()> {
        self.run(&["enable", unit])
    }

    fn disable(&self, unit: &str) -> Result<()> {
        // A unit that was never enabled is not a failure to disable.
        let _ = self.run(&["disable", unit]);
        Ok(())
    }

    fn daemon_reload(&self) -> Result<()> {
        self.run(&["daemon-reload"])
    }

    fn recent_log(&self, unit: &str, lines: u32) -> Option<String> {
        let out = std::process::Command::new("journalctl")
            .args(["-u", unit, "-n", &lines.to_string(), "--no-pager"])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// A scripted `ServiceManager` for tests.
///
/// State can be queued so that successive polls see different answers, which is
/// how "start returned 0, then the unit failed a moment later" is reproduced.
#[cfg(any(test, feature = "testkit"))]
pub mod fake {
    use std::sync::Mutex;
    use std::time::Duration;

    use super::{Result, ServiceManager, UnitState};

    #[derive(Debug)]
    pub struct FakeService {
        /// States to return, consumed one per `state()` call. The last one
        /// repeats forever once the queue is empty.
        states: Mutex<Vec<UnitState>>,
        current: Mutex<UnitState>,
        calls: Mutex<Vec<String>>,
        slept: Mutex<Duration>,
    }

    impl FakeService {
        pub fn new(initial: UnitState) -> Self {
            Self {
                states: Mutex::new(Vec::new()),
                current: Mutex::new(initial),
                calls: Mutex::new(Vec::new()),
                slept: Mutex::new(Duration::ZERO),
            }
        }

        /// Queue states for successive `state()` calls, oldest first.
        pub fn script(self, states: impl IntoIterator<Item = UnitState>) -> Self {
            if let Ok(mut queue) = self.states.lock() {
                queue.extend(states);
                queue.reverse(); // pop() takes from the end
            }
            self
        }

        /// Every systemctl-equivalent call made, in order.
        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().map(|c| c.clone()).unwrap_or_default()
        }

        /// Total time the code under test believed it slept for. Lets a test
        /// assert a countdown ran its full length without waiting for it.
        pub fn slept(&self) -> Duration {
            self.slept.lock().map(|d| *d).unwrap_or_default()
        }

        fn record(&self, call: impl Into<String>) {
            if let Ok(mut calls) = self.calls.lock() {
                calls.push(call.into());
            }
        }

        fn set(&self, state: UnitState) {
            if let Ok(mut current) = self.current.lock() {
                *current = state;
            }
        }
    }

    impl ServiceManager for FakeService {
        fn state(&self, _unit: &str) -> UnitState {
            if let Ok(mut queue) = self.states.lock()
                && let Some(next) = queue.pop()
            {
                self.set(next);
                return next;
            }
            self.current
                .lock()
                .map(|s| *s)
                .unwrap_or(UnitState::Inactive)
        }

        fn start(&self, unit: &str) -> Result<()> {
            self.record(format!("start {unit}"));
            // Mirrors the real thing: `systemctl start` on a Type=simple unit
            // leaves it active as soon as the process forks. A test that wants
            // the interesting case — active, then failed a moment later —
            // queues it with `script()`, which takes precedence over this.
            self.set(UnitState::Active);
            Ok(())
        }

        fn stop(&self, unit: &str) -> Result<()> {
            self.record(format!("stop {unit}"));
            self.set(UnitState::Inactive);
            Ok(())
        }

        fn enable(&self, unit: &str) -> Result<()> {
            self.record(format!("enable {unit}"));
            Ok(())
        }

        fn disable(&self, unit: &str) -> Result<()> {
            self.record(format!("disable {unit}"));
            Ok(())
        }

        fn daemon_reload(&self) -> Result<()> {
            self.record("daemon-reload");
            Ok(())
        }

        fn recent_log(&self, _unit: &str, _lines: u32) -> Option<String> {
            Some("[fake journal]".to_string())
        }

        fn sleep(&self, duration: Duration) {
            if let Ok(mut slept) = self.slept.lock() {
                *slept += duration;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scripted_fake_reproduces_type_simple_optimism() {
        // The case a container cannot produce on demand: `systemctl start`
        // returns 0, and the unit is failed by the time anything looks.
        let svc = fake::FakeService::new(UnitState::Inactive)
            .script([UnitState::Active, UnitState::Failed]);

        svc.start("minecraft").unwrap();
        assert_eq!(svc.state("minecraft"), UnitState::Active);
        assert_eq!(svc.state("minecraft"), UnitState::Failed);
        // The last scripted state repeats rather than falling back.
        assert_eq!(svc.state("minecraft"), UnitState::Failed);
        assert_eq!(svc.calls(), vec!["start minecraft"]);
    }

    #[test]
    fn sleeps_are_recorded_rather_than_taken() {
        let svc = fake::FakeService::new(UnitState::Active);
        svc.sleep(Duration::from_secs(300));
        svc.sleep(Duration::from_secs(60));
        assert_eq!(svc.slept(), Duration::from_secs(360));
    }

    #[test]
    fn systemctl_is_a_no_op_where_systemd_is_not_running() {
        // Not an error: this package installs in containers and build hosts.
        let svc = Systemctl::new(false);
        assert_eq!(svc.state("minecraft"), UnitState::Absent);
        svc.start("minecraft").unwrap();
        svc.daemon_reload().unwrap();
    }
}
