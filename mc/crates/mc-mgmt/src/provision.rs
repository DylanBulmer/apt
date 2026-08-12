//! Switching the management protocol on, and choosing what it listens on.
//!
//! ## mc generates the secret
//!
//! The server will invent one itself when `management-server-secret` is empty,
//! but mc writes one instead, for three reasons: it works before the first
//! start, so `mc mgmt enable` can tell an operator the endpoint immediately;
//! it does not depend on the server writing its choice back to a file mc can
//! read; and a secret mc owns is one core can re-apply after a modpack merges
//! its own `server.properties`, which is what stops a pack choosing it.
//!
//! ## and pins the port
//!
//! `management-server-port = 0` tells the server to pick a random one, which
//! is useless to a client — there is nowhere to read the choice back from. The
//! port is pinned at the game port + 20, leaving RCON's + 10 alone.

use std::io::Read as _;

use mc_common::error::{Error, Result};
use mc_common::paths::{Paths, STOCK_PORT};
use mc_common::properties::{self, Properties};

use crate::endpoint;

/// Offset from the game port. RCON already owns + 10.
pub const PORT_OFFSET: u16 = 20;

/// The port the endpoint should listen on.
///
/// Resolved the same way `properties::rcon_port` resolves RCON's: an explicit
/// setting wins, then the game port plus an offset, then the stock port plus
/// the offset for a server that has no `server.properties` yet.
pub fn port(props: &Properties) -> u16 {
    if let Some(explicit) = props
        .get(endpoint::PORT)
        .and_then(|p| p.trim().parse::<u16>().ok())
        && explicit != 0
    {
        return explicit;
    }
    let game = props
        .get("server-port")
        .and_then(|p| p.trim().parse::<u16>().ok())
        .unwrap_or(STOCK_PORT);
    game.saturating_add(PORT_OFFSET)
}

/// 40 alphanumeric characters, the shape the protocol specifies.
///
/// Rejection-sampled so every character is equally likely: taking a byte
/// modulo 62 would make the first four letters of the alphabet measurably more
/// common, which is a real if small reduction in a secret's strength.
///
/// The charset is also load-bearing beyond the spec: no `=`, which would be
/// ambiguous in a `key=value` properties line, and nothing a shell would treat
/// as a metacharacter.
pub fn generate_secret() -> Result<String> {
    const ALPHABET: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    const LENGTH: usize = 40;

    let mut urandom =
        std::fs::File::open("/dev/urandom").map_err(|e| Error::io("/dev/urandom", e))?;
    let mut secret = String::with_capacity(LENGTH);
    let mut buffer = [0u8; 64];

    while secret.len() < LENGTH {
        urandom
            .read_exact(&mut buffer)
            .map_err(|e| Error::io("/dev/urandom", e))?;
        for byte in buffer {
            // 248 = 4 * 62. Anything above it would bias the result, so it is
            // discarded rather than folded in.
            if byte < 248
                && secret.len() < LENGTH
                && let Some(c) = ALPHABET.get(usize::from(byte % 62))
            {
                secret.push(char::from(*c));
            }
        }
    }
    Ok(secret)
}

/// Switch the protocol on, provisioning a secret and pinning a port.
///
/// Returns whether anything changed, so a caller can avoid recommending a
/// restart for a no-op. The server reads `server.properties` at startup, so a
/// change here needs one before it takes effect.
pub fn enable(paths: &Paths) -> Result<bool> {
    let file = paths.server_properties();
    let mut props = Properties::load(&file);
    let before = props.clone();

    let port = port(&props);
    props.set(endpoint::ENABLED, "true");
    // Loopback by default. The protocol authenticates every connection, so
    // binding elsewhere is a legitimate choice — but it should be one an
    // operator makes deliberately, not one mc makes for them. An existing
    // host or TLS setting is preserved: resetting them on every upgrade would
    // undo a deliberate deployment behind a TLS-terminating proxy.
    if props
        .get(endpoint::HOST)
        .is_none_or(|h| h.trim().is_empty())
    {
        props.set(endpoint::HOST, "localhost");
    }
    props.set(endpoint::PORT, &port.to_string());
    if props.get(endpoint::TLS).is_none_or(|t| t.trim().is_empty()) {
        // TLS off because the endpoint is loopback: there is no network segment to
        // protect, and the protocol's own default would need a PKCS12 keystore to
        // generate, rotate and expire. Terminate TLS at a reverse proxy if the
        // endpoint is ever exposed.
        props.set(endpoint::TLS, "false");
    }

    // An existing secret is kept: re-running enable must not invalidate every
    // client that already holds one.
    if props
        .get(endpoint::SECRET)
        .is_none_or(|s| s.trim().is_empty())
    {
        props.set(endpoint::SECRET, &generate_secret()?);
    }

    if props == before {
        return Ok(false);
    }
    props.save(&file)?;
    // 0640 minecraft:minecraft, always: this file now holds a second secret,
    // and it is only readable at all because the owner is the service account.
    properties::secure(&file)?;
    Ok(true)
}

/// Switch the protocol off, leaving the secret in place.
///
/// The secret survives so that re-enabling restores the same endpoint rather
/// than invalidating whatever holds it — the same reason `mc rcon disable`
/// leaves its password file alone.
pub fn disable(paths: &Paths) -> Result<bool> {
    let file = paths.server_properties();
    let mut props = Properties::load(&file);
    let before = props.clone();

    props.set(endpoint::ENABLED, "false");
    if props == before {
        return Ok(false);
    }
    props.save(&file)?;
    properties::secure(&file)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(pairs: &[(&str, &str)]) -> Properties {
        let mut props = Properties::default();
        for (key, value) in pairs {
            props.set(key, value);
        }
        props
    }

    #[test]
    fn the_port_follows_the_game_port_without_colliding_with_rcon() {
        // RCON is game + 10. Landing on the same port would make whichever
        // service started second fail to bind, with no clue as to why.
        let p = props(&[("server-port", "25565")]);
        assert_eq!(port(&p), 25585);

        let p = props(&[("server-port", "30000")]);
        assert_eq!(port(&p), 30020);
        assert_ne!(port(&p), 30010, "must not be RCON's offset");
    }

    #[test]
    fn a_server_with_no_properties_yet_still_resolves_a_port() {
        assert_eq!(port(&props(&[])), STOCK_PORT + PORT_OFFSET);
    }

    #[test]
    fn an_explicit_port_wins_but_zero_does_not() {
        assert_eq!(port(&props(&[(endpoint::PORT, "9999")])), 9999);
        // 0 means "pick one at random", which no client can discover.
        assert_eq!(
            port(&props(&[(endpoint::PORT, "0"), ("server-port", "25565")])),
            25585
        );
    }

    #[test]
    fn the_secret_is_forty_alphanumeric_characters() {
        let secret = generate_secret().expect("/dev/urandom");
        assert_eq!(secret.len(), 40);
        assert!(
            secret.chars().all(|c| c.is_ascii_alphanumeric()),
            "{secret}"
        );
    }

    #[test]
    fn two_secrets_are_not_the_same_secret() {
        let a = generate_secret().expect("/dev/urandom");
        let b = generate_secret().expect("/dev/urandom");
        assert_ne!(a, b);
    }

    #[test]
    fn a_secret_never_contains_a_character_a_properties_line_would_mangle() {
        // '=' would split a key=value line, and a shell metacharacter would
        // have to be escaped by everything downstream that touches it.
        for _ in 0..16 {
            let secret = generate_secret().expect("/dev/urandom");
            assert!(
                !secret.contains(['=', '\\', '"', '\'', '$', '`', ' ', '\n']),
                "{secret}"
            );
        }
    }

    #[test]
    fn enabling_twice_keeps_the_first_secret() {
        // Re-running enable must not invalidate a client that already holds
        // the secret.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::with_root(dir.path());
        std::fs::create_dir_all(paths.base()).expect("base");

        assert!(enable(&paths).expect("first enable"));
        let first = Properties::load(&paths.server_properties())
            .get(endpoint::SECRET)
            .map(str::to_string);

        assert!(
            !enable(&paths).expect("second enable"),
            "no-op the second time"
        );
        let second = Properties::load(&paths.server_properties())
            .get(endpoint::SECRET)
            .map(str::to_string);

        assert_eq!(first, second);
        assert!(first.is_some_and(|s| s.len() == 40));
    }

    #[test]
    fn disabling_leaves_the_secret_so_enabling_restores_the_same_endpoint() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::with_root(dir.path());
        std::fs::create_dir_all(paths.base()).expect("base");

        enable(&paths).expect("enable");
        let before = Properties::load(&paths.server_properties())
            .get(endpoint::SECRET)
            .map(str::to_string);

        assert!(disable(&paths).expect("disable"));
        let after = Properties::load(&paths.server_properties())
            .get(endpoint::SECRET)
            .map(str::to_string);

        assert_eq!(before, after, "the secret survives being switched off");
        assert_eq!(
            Properties::load(&paths.server_properties()).get(endpoint::ENABLED),
            Some("false")
        );
    }

    #[test]
    fn an_enabled_server_resolves_to_a_loopback_endpoint_with_tls_off() {
        // The shape the rest of the plugin assumes: ws:// on localhost.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::with_root(dir.path());
        std::fs::create_dir_all(paths.base()).expect("base");

        enable(&paths).expect("enable");
        let props = Properties::load(&paths.server_properties());
        let resolved = endpoint::resolve(&props).expect("an endpoint");

        assert!(resolved.url.starts_with("ws://localhost:"));
        assert!(resolved.is_loopback());
        assert_eq!(resolved.secret.len(), 40);
    }

    #[test]
    fn a_second_enable_call_preserves_an_existing_host_and_tls_setting() {
        // An operator running 0.0.0.0 behind a TLS-terminating proxy should not
        // have both keys reset by the next apt upgrade of mc-mgmt.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::with_root(dir.path());
        std::fs::create_dir_all(paths.base()).expect("base");

        enable(&paths).expect("first enable");

        // Simulate an operator changing the host and enabling TLS.
        let mut props = Properties::load(&paths.server_properties());
        props.set(endpoint::HOST, "0.0.0.0");
        props.set(endpoint::TLS, "true");
        props.save(&paths.server_properties()).expect("save");

        // A second enable (e.g. post-upgrade) must not overwrite them.
        enable(&paths).expect("second enable");
        let props = Properties::load(&paths.server_properties());

        assert_eq!(
            props.get(endpoint::HOST),
            Some("0.0.0.0"),
            "host must survive a second enable"
        );
        assert_eq!(
            props.get(endpoint::TLS),
            Some("true"),
            "tls must survive a second enable"
        );
    }
}
