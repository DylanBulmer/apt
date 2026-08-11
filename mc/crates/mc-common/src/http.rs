//! Network access, behind a trait.
//!
//! The trait exists so that version resolution and artifact download are
//! testable offline. Every upstream this talks to (Mojang, PaperMC, FabricMC,
//! NeoForge, Modrinth) has changed shape at least once, and a suite that can
//! only exercise those paths by reaching the real API is a suite that is slow,
//! flaky, and unavailable in a sandbox. The live APIs are still exercised — by
//! the opt-in install-type matrix, which is what catches upstream drift — but
//! nothing else needs them.

use std::path::Path;

use crate::error::{Error, IoContext, Result};

pub trait Http: Send + Sync {
    /// Fetch a URL into memory. For API responses, not artifacts.
    fn get(&self, url: &str) -> Result<Vec<u8>>;

    /// Stream a URL to a file. Artifacts are hundreds of megabytes and this
    /// runs on machines sized for a game server, so it must not buffer.
    fn download(&self, url: &str, dest: &Path) -> Result<()>;

    fn get_string(&self, url: &str) -> Result<String> {
        let bytes = self.get(url)?;
        String::from_utf8(bytes).map_err(|e| Error::Network(format!("{url}: invalid UTF-8: {e}")))
    }
}

/// Reject a URL that is not an `https` URL on an allowlisted host.
///
/// THE MATCH IS ANCHORED AT THE HOST, not searched for anywhere in the string.
/// The shell version extracted the host from the first `https://` found
/// *anywhere*, so a value like `-Ksomefile#https://cdn.modrinth.com/x` passed
/// the allowlist and was then handed to curl as an argv element beginning with
/// `-` — option injection, in a process running as root. Parsing the URL
/// properly rejects that outright.
///
/// Also rejects a host that merely *ends with* an allowlisted name:
/// `cdn.modrinth.com.evil.test` must not pass because it contains
/// `cdn.modrinth.com`.
pub fn host_allowed(url: &str, allowed: &[&str]) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    // Everything from the first '/', '?' or '#' is path/query/fragment.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // Userinfo would let `evil.test@cdn.modrinth.com` read as an allowed host
    // to a careless parser and as `evil.test` to some clients. Refuse outright.
    if authority.contains('@') {
        return false;
    }
    // A port is allowed syntactically but nothing we fetch from uses one, and
    // permitting it widens the target for no benefit.
    let host = authority;
    allowed.iter().any(|a| host.eq_ignore_ascii_case(a))
}

/// The real client: `ureq` over `rustls`.
///
/// rustls rather than native-tls so the `.deb` needs no OpenSSL at runtime —
/// `Depends:` collapses to `${shlibs:Depends}`, and a distribution OpenSSL
/// upgrade can never break the download path.
pub struct UreqHttp {
    agent: ureq::Agent,
}

impl Default for UreqHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl UreqHttp {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(300)))
            .timeout_connect(Some(std::time::Duration::from_secs(20)))
            // Redirects are followed, but only a few: an upstream that
            // redirects indefinitely should fail rather than hang a systemd
            // job.
            .max_redirects(5)
            .user_agent(concat!("mc/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: config.into(),
        }
    }
}

impl Http for UreqHttp {
    fn get(&self, url: &str) -> Result<Vec<u8>> {
        let mut response = self
            .agent
            .get(url)
            .call()
            .map_err(|e| Error::Network(format!("{url}: {e}")))?;
        response
            .body_mut()
            // Bounded: an API response is kilobytes. Without a cap a hostile or
            // broken origin can exhaust memory on a server sized for a JVM.
            .with_config()
            .limit(32 * 1024 * 1024)
            .read_to_vec()
            .map_err(|e| Error::Network(format!("{url}: {e}")))
    }

    fn download(&self, url: &str, dest: &Path) -> Result<()> {
        let mut response = self
            .agent
            .get(url)
            .call()
            .map_err(|e| Error::Network(format!("{url}: {e}")))?;

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).at(parent)?;
        }
        let mut file = std::fs::File::create(dest).at(dest)?;
        let mut reader = response.body_mut().as_reader();
        std::io::copy(&mut reader, &mut file).map_err(|e| {
            // A partial file is worse than none: a later run could mistake it
            // for a cached artifact, and hash verification would reject it with
            // a confusing message about the wrong file.
            let _ = std::fs::remove_file(dest);
            Error::Network(format!("{url}: {e}"))
        })?;
        Ok(())
    }
}

/// A scripted `Http` for tests: routes are registered up front and every
/// request is recorded.
///
/// An unregistered URL is an ERROR, never an empty success — a test whose
/// fixture URL drifted must fail rather than silently exercise the "upstream
/// returned nothing" path it did not mean to test.
#[cfg(any(test, feature = "testkit"))]
pub mod fake {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Mutex;

    use super::{Http, Result};
    use crate::error::{Error, IoContext};

    #[derive(Default)]
    pub struct FakeHttp {
        routes: Mutex<HashMap<String, Vec<u8>>>,
        requests: Mutex<Vec<String>>,
        /// URLs that should fail, for testing the abort paths.
        failures: Mutex<HashMap<String, String>>,
    }

    impl FakeHttp {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn route(self, url: impl Into<String>, body: impl Into<Vec<u8>>) -> Self {
            if let Ok(mut routes) = self.routes.lock() {
                routes.insert(url.into(), body.into());
            }
            self
        }

        pub fn fail(self, url: impl Into<String>, message: impl Into<String>) -> Self {
            if let Ok(mut failures) = self.failures.lock() {
                failures.insert(url.into(), message.into());
            }
            self
        }

        /// Every URL requested, in order. Lets a test assert that a no-op
        /// upgrade never reached the network at all.
        pub fn requests(&self) -> Vec<String> {
            self.requests.lock().map(|r| r.clone()).unwrap_or_default()
        }

        fn lookup(&self, url: &str) -> Result<Vec<u8>> {
            if let Ok(mut log) = self.requests.lock() {
                log.push(url.to_string());
            }
            if let Ok(failures) = self.failures.lock()
                && let Some(message) = failures.get(url)
            {
                return Err(Error::Network(format!("{url}: {message}")));
            }
            self.routes
                .lock()
                .ok()
                .and_then(|routes| routes.get(url).cloned())
                .ok_or_else(|| Error::Network(format!("FakeHttp: no route registered for {url}")))
        }
    }

    impl Http for FakeHttp {
        fn get(&self, url: &str) -> Result<Vec<u8>> {
            self.lookup(url)
        }

        fn download(&self, url: &str, dest: &Path) -> Result<()> {
            let body = self.lookup(url)?;
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).at(parent)?;
            }
            std::fs::write(dest, body).at(dest)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALLOWED: [&str; 2] = ["cdn.modrinth.com", "github.com"];

    #[test]
    fn accepts_the_hosts_it_is_given() {
        assert!(host_allowed(
            "https://cdn.modrinth.com/data/x/y.jar",
            &ALLOWED
        ));
        assert!(host_allowed(
            "https://github.com/o/r/releases/download/v1/x.jar",
            &ALLOWED
        ));
        // Host comparison is case-insensitive, as DNS is.
        assert!(host_allowed("https://CDN.Modrinth.COM/x", &ALLOWED));
    }

    #[test]
    fn rejects_option_injection() {
        // The exact shape that defeated the previous implementation: it found
        // "https://" anywhere in the string, extracted an allowed host, and
        // then handed the whole value to curl as an argv element starting
        // with '-'.
        assert!(!host_allowed(
            "-Ksomefile#https://cdn.modrinth.com/x",
            &ALLOWED
        ));
        assert!(!host_allowed(
            "--output=/etc/cron.d/x https://cdn.modrinth.com/y",
            &ALLOWED
        ));
    }

    #[test]
    fn rejects_a_host_that_merely_contains_an_allowed_name() {
        assert!(!host_allowed(
            "https://cdn.modrinth.com.evil.test/x",
            &ALLOWED
        ));
        assert!(!host_allowed("https://evilcdn.modrinth.com/x", &ALLOWED));
        assert!(!host_allowed(
            "https://evil.test/cdn.modrinth.com/x",
            &ALLOWED
        ));
    }

    #[test]
    fn rejects_userinfo_that_disguises_the_real_host() {
        assert!(!host_allowed(
            "https://cdn.modrinth.com@evil.test/x",
            &ALLOWED
        ));
        assert!(!host_allowed(
            "https://evil.test@cdn.modrinth.com/x",
            &ALLOWED
        ));
    }

    #[test]
    fn rejects_anything_that_is_not_https() {
        assert!(!host_allowed("http://cdn.modrinth.com/x", &ALLOWED));
        assert!(!host_allowed("file:///etc/passwd", &ALLOWED));
        assert!(!host_allowed("ftp://cdn.modrinth.com/x", &ALLOWED));
        assert!(!host_allowed("", &ALLOWED));
        assert!(!host_allowed("cdn.modrinth.com/x", &ALLOWED));
    }

    #[test]
    fn rejects_a_port_even_on_an_allowed_host() {
        assert!(!host_allowed("https://cdn.modrinth.com:8443/x", &ALLOWED));
    }

    #[test]
    fn fake_refuses_an_unregistered_url_rather_than_returning_nothing() {
        // A fixture URL that drifted must fail the test, not silently exercise
        // the "upstream returned nothing" path.
        let http = fake::FakeHttp::new().route("https://a.test/x", b"hello".to_vec());
        assert_eq!(http.get_string("https://a.test/x").unwrap(), "hello");
        assert!(http.get("https://a.test/typo").is_err());
        assert_eq!(
            http.requests(),
            vec!["https://a.test/x", "https://a.test/typo"]
        );
    }
}
