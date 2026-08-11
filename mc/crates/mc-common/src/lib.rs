//! Shared library for the `mc` Minecraft server management framework.
//!
//! Linked statically by the core CLI and by every plugin binary. A plugin gets
//! the real properties engine, path-safety checks, URL allowlist and hash
//! verification from here rather than reimplementing them — and takes on no
//! runtime ABI dependency on core's build by doing so. The plugin contract is
//! the manifest and the process boundary; this crate is just shared code.
//!
//! ## The two kinds of configuration
//!
//! [`config`] is *mc's* — how to run the server: build, Java, heap, backup
//! policy, in `/etc/minecraft/config.toml`.
//!
//! [`properties`] is *the server's* — what the game is: port, seed, MOTD,
//! difficulty, RCON, in `/opt/minecraft/server.properties`. The JVM reads and
//! rewrites that file, so nothing here mirrors a value out of it. Read at the
//! point of use, or it can only go stale.

pub mod config;
pub mod error;
pub mod eula;
pub mod fsx;
pub mod hash;
pub mod http;
pub mod java;
pub mod lock;
pub mod packages;
pub mod paths;
pub mod plugin;
pub mod privilege;
pub mod properties;
pub mod service;
pub mod staging;
pub mod ui;
pub mod version;

pub use config::{Config, ServerType};
pub use error::{Error, Result};
pub use paths::Paths;
