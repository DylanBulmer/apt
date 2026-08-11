//! The WebSocket the JSON-RPC rides on.
//!
//! Everything here is I/O; the protocol logic lives in [`crate::rpc`] behind a
//! trait so that none of it needs a socket to be tested.
//!
//! ## Deadlines are not optional
//!
//! This runs inside `mc shutdown`, which systemd bounds with
//! `TimeoutStopSec`. A read with no timeout against a server that has stopped
//! answering is not a slow shutdown, it is a SIGKILL through the JVM's chunk
//! flush. Every socket here carries a timeout, and the TCP connect is bounded
//! separately because connecting to a filtered port is the slowest failure of
//! the lot.

use std::net::TcpStream;
use std::time::Duration;

use tungstenite::client::IntoClientRequest;
use tungstenite::handshake::client::Request;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use mc_common::error::{Error, Result};

use crate::endpoint::Endpoint;

/// Bound on the TCP connect, the TLS handshake and every frame after it.
///
/// Well inside `plugin::PROBE_DEADLINE`, so that a probe fails on its own
/// terms with a legible message rather than being killed by core.
pub const IO_TIMEOUT: Duration = Duration::from_secs(2);

pub struct WebSocketTransport {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
}

/// The upgrade request, with the bearer secret attached.
///
/// The protocol accepts the secret either in `Authorization` or smuggled
/// through `Sec-WebSocket-Protocol`; the header is used here because the
/// second form exists for browsers, which cannot set headers on a WebSocket.
fn build_request(endpoint: &Endpoint) -> Result<Request> {
    let mut request = endpoint
        .url
        .as_str()
        .into_client_request()
        .map_err(|e| Error::other(format!("{}: {e}", endpoint.url)))?;

    let bearer = format!("Bearer {}", endpoint.secret).parse().map_err(|_| {
        Error::config("the management secret contains characters a header cannot carry")
    })?;
    request.headers_mut().insert("Authorization", bearer);
    Ok(request)
}

impl WebSocketTransport {
    /// Connect, authenticate and hand back a ready transport.
    pub fn connect(endpoint: &Endpoint) -> Result<Self> {
        // Refuse to send the bearer secret in cleartext to a non-loopback host.
        // `mc mgmt status` warns about a non-loopback endpoint, but this is the
        // one place every caller (hooks, subcommands, probe) passes through, and
        // a warning on only one path leaves the secret on the wire everywhere else.
        if !endpoint.is_loopback() && endpoint.url.starts_with("ws://") {
            return Err(Error::config(format!(
                "{} is a non-loopback endpoint without TLS.\n\
                 The bearer secret would be sent in cleartext.\n\
                 Fix: enable TLS (management-server-tls-enabled=true), \
                 or bind to loopback (management-server-host=localhost)",
                endpoint.url
            )));
        }

        let request = build_request(endpoint)?;

        // Connected by hand rather than through `tungstenite::connect` so the
        // timeouts above apply: that helper resolves and dials with no bound
        // at all.
        let address = (endpoint.host.trim_matches(['[', ']']), endpoint.port);
        let stream = std::net::ToSocketAddrs::to_socket_addrs(&address)
            .map_err(|e| Error::other(format!("resolving {}: {e}", endpoint.host)))?
            .next()
            .ok_or_else(|| Error::other(format!("{} resolves to nothing", endpoint.host)))?;

        let stream = TcpStream::connect_timeout(&stream, IO_TIMEOUT)
            .map_err(|e| Error::other(format!("connecting to {}: {e}", endpoint.url)))?;
        stream
            .set_read_timeout(Some(IO_TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(IO_TIMEOUT)))
            .map_err(|e| Error::other(format!("setting socket timeouts: {e}")))?;

        let (socket, _response) =
            tungstenite::client_tls(request, stream).map_err(|e| match e {
                tungstenite::handshake::HandshakeError::Failure(error) => {
                    handshake_error(endpoint, error)
                }
                // Only reachable on a non-blocking stream, which this is not.
                tungstenite::handshake::HandshakeError::Interrupted(_) => {
                    Error::other(format!("{}: handshake interrupted", endpoint.url))
                }
            })?;
        Ok(Self { socket })
    }
}

/// Turn a handshake failure into something an operator can act on.
fn handshake_error(endpoint: &Endpoint, error: tungstenite::Error) -> Error {
    if let tungstenite::Error::Http(response) = &error {
        // 401 is the one an operator actually hits: server.properties and the
        // secret mc holds have drifted apart.
        if response.status() == 401 {
            return Error::config(format!(
                "{} refused the management secret.\n       \
                 server.properties and mc disagree — re-provision with: mc mgmt enable",
                endpoint.url
            ));
        }
        return Error::other(format!(
            "{} refused the upgrade: {}",
            endpoint.url,
            response.status()
        ));
    }
    Error::other(format!("{}: {error}", endpoint.url))
}

impl crate::rpc::Transport for WebSocketTransport {
    fn send(&mut self, text: &str) -> Result<()> {
        self.socket
            .send(Message::text(text))
            .map_err(|e| Error::other(format!("sending: {e}")))
    }

    fn recv(&mut self) -> Result<String> {
        loop {
            let message = self
                .socket
                .read()
                .map_err(|e| Error::other(format!("reading: {e}")))?;

            match message {
                Message::Text(text) => return Ok(text.to_string()),
                // Ping/pong are answered by tungstenite itself on the next
                // write; binary frames are not part of this protocol.
                Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => {}
                Message::Close(_) => {
                    return Err(Error::other("the server closed the connection".to_string()));
                }
            }
        }
    }
}

impl Drop for WebSocketTransport {
    fn drop(&mut self) {
        // A courtesy close so the server does not log a truncated connection
        // every time a hook finishes. Failure is irrelevant — the process is
        // on its way out.
        let _ = self.socket.close(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(secret: &str) -> Endpoint {
        Endpoint {
            url: "ws://localhost:25585".to_string(),
            secret: secret.to_string(),
            host: "localhost".to_string(),
            port: 25585,
        }
    }

    #[test]
    fn the_secret_travels_in_the_authorization_header() {
        let request = build_request(&endpoint("s3cret")).unwrap();
        assert_eq!(
            request.headers().get("Authorization").unwrap(),
            "Bearer s3cret"
        );
    }

    #[test]
    fn a_secret_that_cannot_be_a_header_is_refused_before_it_is_sent() {
        // The server generates a 40-character alphanumeric secret, so this is
        // only reachable through a hand-edited server.properties — but a
        // newline here would otherwise be a header injection.
        let error = build_request(&endpoint("bad\nvalue")).expect_err("refused");
        assert_eq!(error.exit_code(), 78, "operator-fixable");
    }

    #[test]
    fn a_refused_secret_says_what_to_run() {
        let response = tungstenite::http::Response::builder()
            .status(401)
            .body(None::<Vec<u8>>)
            .unwrap();
        let error = handshake_error(&endpoint("s"), tungstenite::Error::Http(Box::new(response)));
        assert!(error.to_string().contains("mc mgmt enable"), "{error}");
    }

    #[test]
    fn a_non_loopback_plaintext_endpoint_is_refused() {
        let endpoint = Endpoint {
            url: "ws://10.0.0.5:25585".to_string(),
            secret: "s3cret".to_string(),
            host: "10.0.0.5".to_string(),
            port: 25585,
        };
        let error = match WebSocketTransport::connect(&endpoint) {
            Err(e) => e,
            Ok(_) => panic!("expected error for non-loopback plaintext"),
        };
        assert!(error.to_string().contains("cleartext"), "{error}");
        assert_eq!(error.exit_code(), 78, "operator-fixable");
    }

    #[test]
    fn a_loopback_plaintext_endpoint_is_allowed() {
        // ws://localhost is fine — no network segment to protect.
        // This fails at TCP connect (no server), not at our check.
        let endpoint = Endpoint {
            url: "ws://localhost:25585".to_string(),
            secret: "s3cret".to_string(),
            host: "localhost".to_string(),
            port: 25585,
        };
        let error = match WebSocketTransport::connect(&endpoint) {
            Err(e) => e,
            Ok(_) => panic!("expected error (no server) for loopback plaintext"),
        };
        assert!(!error.to_string().contains("cleartext"), "{error}");
    }
}
