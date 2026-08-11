//! The `mc` dispatcher, as a library.
//!
//! A library as well as a binary so that integration tests can construct a
//! [`context::Ctx`] with a temp root, a scripted HTTP client and a scripted
//! service manager, and then call the REAL command handlers. Testing the
//! handlers through the binary instead would mean a subprocess per case and no
//! way to script systemd's timing — which is exactly where the interesting bugs
//! are.

pub mod cli;
pub mod commands;
pub mod context;
pub mod dispatch;
pub mod sources;
