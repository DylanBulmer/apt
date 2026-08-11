//! What a console does to a running server, independent of how it talks to it.
//!
//! Two plugins implement [`Console`]: `mc-rcon` over RCON, `mc-mgmt` over the
//! management protocol. Everything in [`hooks`] is written once against the
//! trait, so the shutdown policy — who is warned, in what order, what happens
//! when the count cannot be obtained — cannot drift between them.
//!
//! Deliberately NOT in `mc-common`. That crate is the contract every package
//! depends on, including `mc`, `mc-backup` and `mc-mrpack`, none of which have
//! any business knowing what a countdown is. Console policy is a different
//! axis, needed by exactly the packages that implement one.
//!
//! ## The transport is the only difference
//!
//! | policy step | RCON | management protocol |
//! |---|---|---|
//! | count | `list`, then parse prose | `minecraft:players/` |
//! | announce | `tellraw` | `minecraft:server/system_message` |
//! | stop | `stop` | `minecraft:server/stop` |
//! | hold saves | `save-off` + `save-all` | `autosave/set false` + `save` |
//! | resume saves | `save-on` | `autosave/set true` |

pub mod countdown;
pub mod hooks;

use std::process::ExitCode;

use mc_common::error::Result;

pub use countdown::PlayerCount;

/// Answer `<bin> console <verb>`, the entry point core elects through.
///
/// THE ANSWER IS THE EXIT STATUS AND NOTHING ELSE. Core probes every installed
/// console on the shutdown path, and a console that is simply switched off is
/// not a fault worth a line in the journal each time the server stops. Printing
/// here would put one there.
///
/// `usable` is only called for the verb it applies to, so a console pays for a
/// connection attempt only when something is actually asking.
pub fn answer_probe(verb: &str, usable: impl FnOnce() -> bool) -> ExitCode {
    if probe_succeeds(verb, usable) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The decision behind [`answer_probe`], separated because `ExitCode` cannot
/// be compared in a test.
fn probe_succeeds(verb: &str, usable: impl FnOnce() -> bool) -> bool {
    if verb != "probe" {
        // A verb core did not send. Reported on stderr rather than silently
        // failing, because it means the two sides of the contract disagree.
        eprintln!("mc: unknown console verb '{verb}'");
        return false;
    }
    usable()
}

/// The operations a console must provide for mc's lifecycle hooks.
///
/// Six methods, chosen as the smallest set that covers every hook. A console
/// that can do these can be elected; anything richer is that plugin's own
/// business and belongs on its own type.
pub trait Console {
    /// Broadcast to everyone on the server.
    fn say(&mut self, message: &str) -> Result<()>;

    /// How many players are online.
    ///
    /// Returns [`PlayerCount::Unknown`] rather than an error when the question
    /// cannot be answered: not knowing is a supported outcome that the
    /// countdown treats as "assume somebody is there".
    fn player_count(&mut self) -> PlayerCount;

    /// Write the world to disk and wait for it.
    fn save_now(&mut self) -> Result<()>;

    /// Turn periodic saving on or off.
    fn set_autosave(&mut self, enabled: bool) -> Result<()>;

    /// Ask the server to shut itself down.
    fn stop(&mut self) -> Result<()>;

    /// Wait, as part of pacing a countdown or letting a flush reach disk.
    ///
    /// A method on the trait rather than a bare `thread::sleep` so the policy
    /// in [`hooks`] can be tested at speed. The schedule is five minutes of
    /// real time: a test that actually waited it out would be a test nobody
    /// runs, which is how a shutdown bug reaches production.
    fn wait(&mut self, duration: std::time::Duration) {
        std::thread::sleep(duration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_usable_console_answers_its_probe() {
        assert!(probe_succeeds("probe", || true));
        assert!(!probe_succeeds("probe", || false));
    }

    #[test]
    fn an_unknown_verb_fails_without_consulting_the_console() {
        // Connecting to answer a question core did not ask would put a socket
        // attempt on a path that should be free.
        let mut asked = false;
        let answer = probe_succeeds("interrogate", || {
            asked = true;
            true
        });
        assert!(!answer);
        assert!(!asked, "the console was consulted for a verb it never got");
    }
}
