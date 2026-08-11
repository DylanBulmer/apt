//! The Source RCON wire protocol, as Minecraft implements it.
//!
//! ```text
//! [length:i32][id:i32][type:i32][payload:bytes][pad:\x00\x00]
//! length = 4 + 4 + N + 2  =  PKT_OVERHEAD (10) + N
//! ```
//!
//! All integers little-endian. Auth is type 3 with the password as payload; the
//! server replies with id `-1` if it is denied. A command is type 2.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use mc_common::error::{Error, Result};

pub const AUTH: i32 = 3;
pub const EXEC: i32 = 2;

/// Client→server payload limit, from the protocol documentation.
pub const MAX_PAYLOAD: usize = 1446;
/// Server→client payload limit.
pub const MAX_RESPONSE_PAYLOAD: usize = 4096;
/// id(4) + type(4) + pad(2).
const PKT_OVERHEAD: usize = 10;

/// Correlation tags. They only need to be distinct from each other so the
/// sentinel reply is unambiguous.
const ID_AUTH: i32 = 1;
const ID_CMD: i32 = 2;
const ID_SENTINEL: i32 = 3;

/// Per-read timeout.
const IO_TIMEOUT: Duration = Duration::from_secs(10);

// Bounds on multi-packet reassembly. Because reading continues until the
// sentinel reply arrives, a hostile or malfunctioning server could otherwise
// stream packets forever.
//
//   MAX_TOTAL_RESPONSE — real worst cases (`/help` on a heavily modded server)
//     run to a few tens of KiB, so 1 MiB leaves two orders of magnitude of
//     headroom while staying trivially allocatable.
//   MAX_RESPONSE_PKTS — a full packet carries 4096 bytes, so 512 packets could
//     hold more than the byte cap allows. This bound only fires against a
//     server dribbling many small packets while never sending the sentinel.
const MAX_TOTAL_RESPONSE: usize = 1024 * 1024;
const MAX_RESPONSE_PKTS: usize = 512;

/// Hard wall-clock budget for ONE command's entire exchange.
///
/// The two bounds above cap memory but not time: a server sending one packet
/// just inside each per-read timeout would satisfy that limit 512 times over —
/// roughly 85 minutes for a single command. That matters because this client
/// runs from systemd's `ExecStop=`, where overrunning `TimeoutStopSec` means a
/// SIGKILL through the JVM's chunk flush, which is precisely the world
/// corruption a graceful shutdown exists to prevent.
pub const CMD_DEADLINE: Duration = Duration::from_secs(30);

pub struct Connection {
    stream: TcpStream,
}

impl Connection {
    /// Connect, refusing any host that resolves to a non-loopback address.
    ///
    /// RCON IS PLAINTEXT. The password and every command would be on the wire
    /// in clear. The check is applied to EVERY resolved address, not just the
    /// one connected to: a name resolving to both `127.0.0.1` and a public
    /// address must be refused outright rather than accepted because the
    /// loopback candidate happened to be tried first.
    pub fn connect(host: &str, port: u16) -> Result<Self> {
        use std::net::ToSocketAddrs as _;

        let candidates: Vec<std::net::SocketAddr> = (host, port)
            .to_socket_addrs()
            .map_err(|e| Error::other(format!("{host}:{port}: {e}")))?
            .collect();

        if candidates.is_empty() {
            return Err(Error::other(format!("{host}:{port} resolved to nothing.")));
        }
        if let Some(public) = candidates.iter().find(|a| !a.ip().is_loopback()) {
            return Err(Error::denied(format!(
                "Refusing non-loopback host '{host}' (resolves to {}) — RCON is unencrypted \
                 and must only be used over loopback (127.0.0.1 / ::1).",
                public.ip()
            )));
        }

        // Try each candidate: a dual-stack host may have IPv6 unreachable while
        // IPv4 works.
        let mut last = None;
        for address in &candidates {
            match TcpStream::connect_timeout(address, IO_TIMEOUT) {
                Ok(stream) => {
                    stream.set_read_timeout(Some(IO_TIMEOUT)).ok();
                    stream.set_write_timeout(Some(IO_TIMEOUT)).ok();
                    return Ok(Self { stream });
                }
                Err(e) => last = Some(e),
            }
        }
        Err(Error::other(format!(
            "Could not connect to {host}:{port}{}",
            last.map(|e| format!(": {e}")).unwrap_or_default()
        )))
    }

    /// Authenticate. The password is never logged, and never appears in argv.
    pub fn authenticate(&mut self, password: &str) -> Result<()> {
        self.send(ID_AUTH, AUTH, password)?;

        // The auth exchange gets the same budget as a command: a peer dribbling
        // the reply header a byte at a time would otherwise stall here before
        // the caller's real command ever runs.
        let deadline = Instant::now() + CMD_DEADLINE;
        let (id, _type, _payload) = self.recv(deadline)?;

        // The server echoes the request id on success, or replies -1 on
        // failure. Both are checked: the sentinel AND the expected echo.
        if id == -1 {
            return Err(Error::denied("Authentication failed — check the password."));
        }
        if id != ID_AUTH {
            return Err(Error::other(format!(
                "Unexpected response id {id} during authentication."
            )));
        }
        Ok(())
    }

    /// Send a command and return the server's complete response.
    ///
    /// A response longer than [`MAX_RESPONSE_PAYLOAD`] is split across several
    /// packets, and the protocol carries no "last fragment" flag. Reading one
    /// packet would leave the continuations in the socket buffer, where the
    /// NEXT command would misread them as its own reply — every subsequent
    /// response in a session shifted by one, which is far worse than
    /// truncation.
    ///
    /// The fix is a sentinel: an empty command packet with a different id.
    /// Servers answer strictly in order, so the sentinel's reply cannot arrive
    /// before the final fragment of the real response.
    ///
    /// THE SENTINEL IS HELD BACK until the first response packet is in hand,
    /// and that ordering is load-bearing. Minecraft's RCON server does one
    /// `read()` per loop iteration and requires the bytes it got to be exactly
    /// one packet — `length != bytes_read - 4` makes it drop the connection.
    /// Writing the command and the sentinel back to back puts both on the wire
    /// fast enough that TCP hands the server a single segment holding both, and
    /// it hangs up: the session dies partway through, typically on the second
    /// command an operator types. Waiting for the response forces a full round
    /// trip, so each packet arrives in a `read()` of its own.
    pub fn exec(&mut self, command: &str) -> Result<String> {
        if command.len() > MAX_PAYLOAD {
            return Err(Error::rejected(format!(
                "Command is {} bytes; the protocol limit is {MAX_PAYLOAD}.",
                command.len()
            )));
        }

        let deadline = Instant::now() + CMD_DEADLINE;
        self.send(ID_CMD, EXEC, command)?;

        let mut sentinel_sent = false;
        let mut out = String::new();

        for packets in 0.. {
            if packets >= MAX_RESPONSE_PKTS {
                return Err(Error::other(format!(
                    "Server sent {MAX_RESPONSE_PKTS} packets without terminating the response."
                )));
            }

            let (id, _type, payload) = self.recv(deadline)?;

            if id == ID_SENTINEL {
                return Ok(out);
            }

            out.push_str(&payload);
            if out.len() > MAX_TOTAL_RESPONSE {
                return Err(Error::other(format!(
                    "Response exceeded {MAX_TOTAL_RESPONSE} bytes."
                )));
            }

            if !sentinel_sent {
                self.send(ID_SENTINEL, EXEC, "")?;
                sentinel_sent = true;
            }
        }
        unreachable!("the loop above returns or errors")
    }

    fn send(&mut self, id: i32, packet_type: i32, payload: &str) -> Result<()> {
        let body = payload.as_bytes();
        let length = PKT_OVERHEAD + body.len();

        let mut packet = Vec::with_capacity(length + 4);
        packet.extend_from_slice(&i32::try_from(length).unwrap_or(i32::MAX).to_le_bytes());
        packet.extend_from_slice(&id.to_le_bytes());
        packet.extend_from_slice(&packet_type.to_le_bytes());
        packet.extend_from_slice(body);
        // Two terminators: one ends the payload string, one ends the (always
        // empty) second string the protocol defines.
        packet.extend_from_slice(&[0, 0]);

        self.stream
            .write_all(&packet)
            .map_err(|e| Error::other(format!("sending an RCON packet: {e}")))
    }

    fn recv(&mut self, deadline: Instant) -> Result<(i32, i32, String)> {
        let mut length_bytes = [0u8; 4];
        self.read_exact(&mut length_bytes, deadline)?;
        let length = i32::from_le_bytes(length_bytes);

        // A length below the fixed overhead cannot describe a real packet, and
        // one above the documented maximum is either a broken server or an
        // attempt to make us allocate. Both are refused before any allocation.
        let length = usize::try_from(length).map_err(|_| {
            Error::other(format!("Server sent a negative packet length ({length})."))
        })?;
        if !(PKT_OVERHEAD..=MAX_RESPONSE_PAYLOAD + PKT_OVERHEAD).contains(&length) {
            return Err(Error::other(format!(
                "Server sent an implausible packet length ({length})."
            )));
        }

        let mut rest = vec![0u8; length];
        self.read_exact(&mut rest, deadline)?;

        let id = i32::from_le_bytes(take4(&rest, 0)?);
        let packet_type = i32::from_le_bytes(take4(&rest, 4)?);
        // Trailing NULs are protocol padding, not content.
        let body = rest.get(8..).unwrap_or_default();
        let body = body.strip_suffix(&[0, 0]).unwrap_or(body);
        let payload = String::from_utf8_lossy(body).into_owned();

        Ok((id, packet_type, payload))
    }

    /// `read_exact`, but bounded by the command's overall wall-clock budget as
    /// well as by the per-read socket timeout.
    fn read_exact(&mut self, buf: &mut [u8], deadline: Instant) -> Result<()> {
        let mut filled = 0;
        while filled < buf.len() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::other(
                    "RCON exchange exceeded its deadline; the server is not responding.",
                ));
            }
            // Shrink the socket timeout so a slow trickle cannot outlast the
            // overall budget one read at a time.
            self.stream
                .set_read_timeout(Some(remaining.min(IO_TIMEOUT)))
                .ok();

            let Some(slice) = buf.get_mut(filled..) else {
                break;
            };
            match self.stream.read(slice) {
                Ok(0) => {
                    return Err(Error::other("Server closed the connection."));
                }
                Ok(n) => filled += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(Error::other(format!("reading an RCON packet: {e}"))),
            }
        }
        Ok(())
    }
}

fn take4(buf: &[u8], offset: usize) -> Result<[u8; 4]> {
    buf.get(offset..offset + 4)
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
        .ok_or_else(|| Error::other("Server sent a truncated RCON packet."))
}

/// Encode a packet, exposed so tests can build server responses.
pub fn encode(id: i32, packet_type: i32, payload: &str) -> Vec<u8> {
    let body = payload.as_bytes();
    let length = PKT_OVERHEAD + body.len();
    let mut packet = Vec::with_capacity(length + 4);
    packet.extend_from_slice(&i32::try_from(length).unwrap_or(i32::MAX).to_le_bytes());
    packet.extend_from_slice(&id.to_le_bytes());
    packet.extend_from_slice(&packet_type.to_le_bytes());
    packet.extend_from_slice(body);
    packet.extend_from_slice(&[0, 0]);
    packet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_packet_is_framed_as_the_protocol_describes() {
        let packet = encode(2, EXEC, "list");
        // length = 4 (id) + 4 (type) + 4 ("list") + 2 (pad) = 14
        assert_eq!(&packet[0..4], &14i32.to_le_bytes());
        assert_eq!(&packet[4..8], &2i32.to_le_bytes());
        assert_eq!(&packet[8..12], &EXEC.to_le_bytes());
        assert_eq!(&packet[12..16], b"list");
        assert_eq!(&packet[16..18], &[0, 0]);
        assert_eq!(packet.len(), 18);
    }

    #[test]
    fn an_empty_payload_still_carries_both_terminators() {
        let packet = encode(3, EXEC, "");
        assert_eq!(&packet[0..4], &10i32.to_le_bytes());
        assert_eq!(packet.len(), 14);
    }

    #[test]
    fn integers_are_little_endian_regardless_of_host() {
        // The C version had explicit byte-order helpers for this. `to_le_bytes`
        // is the same guarantee, asserted so a refactor cannot lose it.
        let packet = encode(0x01020304, EXEC, "");
        assert_eq!(&packet[4..8], &[0x04, 0x03, 0x02, 0x01]);
    }

    #[test]
    fn loopback_is_the_only_host_this_will_dial() {
        // RCON is plaintext: the password and every command would be in clear
        // on the wire.
        for host in ["example.com", "8.8.8.8", "0.0.0.0"] {
            match Connection::connect(host, 25575) {
                Err(Error::Denied(msg)) => assert!(msg.contains("loopback"), "{msg}"),
                // Resolution failure in a sandbox is acceptable; a *successful*
                // connection to a public address is not.
                Err(_) => {}
                Ok(_) => panic!("{host} should never be dialled"),
            }
        }
    }
}
