//! The one error type shared by the CLI and every plugin.
//!
//! Variants are named for what the *operator* has to do about them, not for the
//! layer that produced them. `mc` maps them to exit codes, and the mapping is
//! part of the contract with systemd — see [`Error::exit_code`].

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Something an operator must fix before this can work: an unaccepted EULA,
    /// an unreadable server.properties, a missing server.jar, a malformed
    /// config file. Maps to exit 78.
    #[error("{0}")]
    Config(String),

    /// The caller does not hold the privileges this command needs.
    #[error("{0}")]
    Denied(String),

    /// Another mc operation holds the lock.
    #[error("{0}")]
    Locked(String),

    /// Refused because a value failed validation. Distinct from `Config`
    /// because these are the paths that take untrusted input — a `.mrpack`
    /// manifest, a backup archive, a pack's server.properties — and the
    /// distinction is what makes a rejection legible in the journal.
    #[error("{0}")]
    Rejected(String),

    #[error("{0}")]
    Network(String),

    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{0}")]
    Other(String),
}

impl Error {
    /// `EX_CONFIG` from sysexits(3).
    ///
    /// LOAD-BEARING, and deliberately distinct from the JVM's own exit codes.
    /// `minecraft.service` maps exactly this value to `RestartPreventExitStatus=`,
    /// so an operator-fixable problem fails visibly without restart-looping,
    /// while every other non-zero exit stays a genuine crash that systemd
    /// restarts. A rule broad enough to cover both would silence real crashes.
    pub const EX_CONFIG: i32 = 78;

    pub fn exit_code(&self) -> i32 {
        match self {
            // Rejected joins Config here on purpose: from systemd's point of
            // view a hostile modpack and a missing jar are the same situation —
            // no amount of restarting fixes either.
            Error::Config(_) | Error::Rejected(_) => Self::EX_CONFIG,
            _ => 1,
        }
    }

    pub fn config(msg: impl Into<String>) -> Self {
        Error::Config(msg.into())
    }

    pub fn denied(msg: impl Into<String>) -> Self {
        Error::Denied(msg.into())
    }

    pub fn rejected(msg: impl Into<String>) -> Self {
        Error::Rejected(msg.into())
    }

    pub fn other(msg: impl Into<String>) -> Self {
        Error::Other(msg.into())
    }

    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}

/// Attach the path to an io::Error, which otherwise reports "No such file or
/// directory" with no indication of which file.
pub trait IoContext<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|source| Error::io(path, source))
    }
}
