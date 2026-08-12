//! What every command is handed.
//!
//! The three injectable seams — paths, HTTP, service manager — are fields here
//! rather than globals, so an integration test drives a real command handler
//! against a temp root with scripted upstreams and a scripted systemd. Nothing
//! below this struct consults the environment.

use std::path::PathBuf;

use mc_common::Paths;
use mc_common::http::Http;
use mc_common::packages::PackageManager;
use mc_common::service::ServiceManager;

pub struct Ctx {
    pub paths: Paths,
    pub http: Box<dyn Http>,
    pub service: Box<dyn ServiceManager>,
    pub packages: Box<dyn PackageManager>,
    /// The invocation as it was typed.
    ///
    /// Captured before any command's own option parsing consumes it: by the
    /// time a privilege guard decides root is needed, the arguments are gone,
    /// and a refusal that cannot echo what the operator typed is a refusal they
    /// have to reconstruct by hand.
    pub argv: Vec<String>,
}

impl Ctx {
    /// The real thing: system paths, a live HTTP client, and systemd if it is
    /// the running init.
    pub fn system(argv: Vec<String>) -> Self {
        let paths = Paths::from_env();
        let systemd = paths.systemd_running();
        Self {
            http: Box::new(mc_common::http::UreqHttp::new()),
            service: Box::new(mc_common::service::Systemctl::new(systemd)),
            packages: Box::new(mc_common::packages::Apt),
            paths,
            argv,
        }
    }

    pub fn mc_bin(&self) -> PathBuf {
        self.paths.mc_bin()
    }
}
