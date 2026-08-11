//! Installing host packages, behind a trait.
//!
//! The fourth injectable seam, and it exists for the same reason as the other
//! three: `mc install` provisions a JRE, and a test that could not intercept
//! that would either have to run as root on a Debian box or skip the step —
//! and skipping it means never asserting that the RIGHT runtime was asked for,
//! which is the part that has been wrong before.

use crate::error::{Error, Result};

pub trait PackageManager: Send + Sync {
    /// Install a package, non-interactively. The caller has already obtained
    /// whatever consent is required.
    fn install(&self, package: &str) -> Result<()>;
}

/// The real implementation: `apt-get`.
pub struct Apt;

impl PackageManager for Apt {
    fn install(&self, package: &str) -> Result<()> {
        let update = std::process::Command::new("apt-get")
            .args(["update", "-qq"])
            .status();
        if update.is_err() {
            return Err(Error::config(format!(
                "Could not run apt-get. Install manually: apt install {package}"
            )));
        }
        let status = std::process::Command::new("apt-get")
            .args(["install", "-y", "--no-install-recommends", package])
            .status()
            .map_err(|e| Error::other(format!("apt-get install: {e}")))?;
        if status.success() {
            Ok(())
        } else {
            Err(Error::config(format!(
                "Failed to install {package}. Install manually: apt install {package}"
            )))
        }
    }
}

/// Records requests instead of making them.
#[cfg(any(test, feature = "testkit"))]
pub mod fake {
    use std::sync::Mutex;

    use super::{PackageManager, Result};

    #[derive(Debug, Default)]
    pub struct FakePackages {
        installed: Mutex<Vec<String>>,
        fail: bool,
    }

    impl FakePackages {
        pub fn new() -> Self {
            Self::default()
        }

        /// A host where the package is unavailable.
        pub fn failing() -> Self {
            Self {
                installed: Mutex::new(Vec::new()),
                fail: true,
            }
        }

        pub fn installed(&self) -> Vec<String> {
            self.installed.lock().map(|i| i.clone()).unwrap_or_default()
        }
    }

    impl PackageManager for FakePackages {
        fn install(&self, package: &str) -> Result<()> {
            if let Ok(mut installed) = self.installed.lock() {
                installed.push(package.to_string());
            }
            if self.fail {
                Err(crate::error::Error::config(format!(
                    "Failed to install {package}."
                )))
            } else {
                Ok(())
            }
        }
    }
}
