//! `mc-mgmt` — the Minecraft Server Management Protocol console.
//!
//! A second console for mc, speaking the JSON-RPC-over-WebSocket protocol
//! Mojang added in 1.21.9 (25w35a). It contributes the same lifecycle hooks as
//! `mc-rcon` and outranks it: core elects the highest-priority console whose
//! probe succeeds, so this one takes over wherever the server is new enough
//! and steps aside everywhere else, with no package conflict between them.
//!
//! ## Why prefer it
//!
//! RCON is a single plaintext password and a command string whose reply is
//! prose. This is a bearer secret, typed methods and typed results: the player
//! count arrives as an array rather than as a sentence that every fork words
//! differently, and there is no command string for a name to be injected into.

pub mod endpoint;
pub mod methods;
pub mod rpc;
pub mod transport;
