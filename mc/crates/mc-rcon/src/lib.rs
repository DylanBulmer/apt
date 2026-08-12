//! `mc-rcon` — the console plugin.
//!
//! Contributes the `mc rcon` subcommand and four hooks. The hooks are what make
//! a stop graceful and a backup consistent, which is why `mc-server`
//! *Recommends* this package: without it `mc stop` disconnects everyone with no
//! warning, and a backup archives a world that was never flushed.

pub mod chat;
pub mod password;
pub mod players;
pub mod protocol;
pub mod session;
