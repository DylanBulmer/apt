//! `mc-backup` — the backup plugin.
//!
//! Contributes `mc backup` and `mc restore`, and owns the systemd timer that
//! runs them on a schedule.
//!
//! It never talks to the running server itself. Flushing the world before an
//! archive and re-enabling saves afterwards are `pre-backup` / `post-backup`
//! hooks, dispatched through `mc-common` — so `mc-rcon` provides them today,
//! and a future console plugin can provide them instead without this package
//! changing.

pub mod archive;
pub mod rotation;
