//! JSON-RPC 2.0 over a message transport.
//!
//! Split from the socket on purpose. Everything interesting here — matching a
//! reply to its request, stepping over notifications that arrive mid-call,
//! turning an error object into a legible message — is testable against a
//! scripted transport, and none of it needs a Minecraft server or a port.
//!
//! ## Notifications interleave with replies
//!
//! The server pushes `minecraft:notification/...` messages whenever it likes,
//! including between a request and its answer. A client that assumed the next
//! frame was its reply would return a player-join event as the result of
//! `server/save` the first time somebody connected during a backup.

use serde::Deserialize;
use serde::de::DeserializeOwned;

use mc_common::error::{Error, Result};

/// A bidirectional stream of text frames.
///
/// The seam the tests inject at, in the same spirit as `mc_common::http::Http`.
pub trait Transport {
    fn send(&mut self, text: &str) -> Result<()>;
    /// Blocks until the next frame arrives.
    fn recv(&mut self) -> Result<String>;
}

/// How many frames a call will step over before giving up on its reply.
///
/// A busy server can push a burst of notifications between the request and the
/// answer, but not an unbounded one — and without a cap, a server that never
/// answers keeps the client blocked past the unit's stop timeout.
const MAX_INTERLEAVED: usize = 64;

pub struct Client<T: Transport> {
    /// Public so a test can assert what actually went on the wire — the only
    /// way to prove a name was sent as data rather than interpolated into a
    /// command string.
    pub transport: T,
    next_id: u64,
}

#[derive(Debug, Deserialize)]
struct Response {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<RpcError>,
    /// Present on a notification, absent on a reply.
    #[serde(default)]
    method: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

/// JSON-RPC's "the method does not exist".
///
/// The protocol is versioned and gained methods over time, so this is how a
/// client tells "this server is too old for that feature" apart from "that
/// feature failed" — see [`Client::supports`].
pub const METHOD_NOT_FOUND: i64 = -32601;

impl<T: Transport> Client<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            next_id: 1,
        }
    }

    /// Call a method and deserialise its result.
    ///
    /// `params` are positional, which is what the server expects: a method
    /// documented as taking `add: Array<Player>` is called with that array as
    /// the single positional parameter.
    pub fn call<R: DeserializeOwned>(
        &mut self,
        method: &str,
        params: Vec<serde_json::Value>,
    ) -> Result<R> {
        let value = self.call_raw(method, params)?;
        serde_json::from_value(value)
            .map_err(|e| Error::other(format!("{method}: unexpected reply shape: {e}")))
    }

    pub fn call_raw(
        &mut self,
        method: &str,
        params: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let id = self.next_id;
        self.next_id += 1;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.transport.send(&request.to_string())?;

        for _ in 0..MAX_INTERLEAVED {
            let frame = self.transport.recv()?;
            let response: Response = serde_json::from_str(&frame)
                .map_err(|e| Error::other(format!("{method}: malformed reply: {e}")))?;

            // A notification carries a method and no id. Stepping over it
            // rather than failing is what lets a call survive a player joining
            // halfway through it.
            if response.id != Some(id) {
                if response.method.is_some() {
                    continue;
                }
                // A reply to an id we never sent, or to one already answered.
                // Skipped for the same reason, but worth saying out loud.
                mc_common::ui::warn(format!(
                    "{method}: ignoring a reply to request {:?}",
                    response.id
                ));
                continue;
            }

            if let Some(error) = response.error {
                return Err(rpc_error(method, &error));
            }
            return Ok(response.result.unwrap_or(serde_json::Value::Null));
        }

        Err(Error::other(format!(
            "{method}: no reply after {MAX_INTERLEAVED} frames"
        )))
    }

    /// Whether the server implements a method.
    ///
    /// Asked by calling it and reading the error code, rather than by parsing
    /// `rpc.discover`: the discovery document is large, and the only question
    /// a caller ever has is about one specific method.
    pub fn is_unsupported(error: &Error) -> bool {
        error.to_string().contains(&format!("[{METHOD_NOT_FOUND}]"))
    }
}

fn rpc_error(method: &str, error: &RpcError) -> Error {
    // The code is carried into the message because it is the only part a
    // caller can branch on, and Error has no room for structured data.
    let message = format!("{method} failed [{}]: {}", error.code, error.message);
    if error.code == METHOD_NOT_FOUND {
        // Not a fault of the operator's: it means this server is older than
        // the feature.
        return Error::config(format!(
            "{message}\n       This server's protocol version does not implement it."
        ));
    }
    Error::other(message)
}

#[cfg(test)]
pub mod fake {
    //! A scripted transport: what the server would have said, in order.

    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct Scripted {
        pub sent: RefCell<Vec<String>>,
        replies: RefCell<Vec<String>>,
    }

    impl Scripted {
        /// Replies are handed out in order, one per `recv`.
        pub fn new(replies: &[&str]) -> Self {
            Self {
                sent: RefCell::new(Vec::new()),
                replies: RefCell::new(replies.iter().rev().map(|s| s.to_string()).collect()),
            }
        }
    }

    impl Transport for Scripted {
        fn send(&mut self, text: &str) -> Result<()> {
            self.sent.borrow_mut().push(text.to_string());
            Ok(())
        }

        fn recv(&mut self) -> Result<String> {
            self.replies
                .borrow_mut()
                .pop()
                .ok_or_else(|| Error::other("the script ran out of replies".to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fake::Scripted;

    #[test]
    fn a_reply_is_matched_to_its_request_by_id() {
        let mut client = Client::new(Scripted::new(&[r#"{"jsonrpc":"2.0","id":1,"result":[]}"#]));
        let players: Vec<serde_json::Value> = client.call("minecraft:players/", vec![]).unwrap();
        assert!(players.is_empty());

        let sent = &client.transport.sent.borrow()[0];
        assert!(sent.contains(r#""method":"minecraft:players/""#), "{sent}");
        assert!(sent.contains(r#""jsonrpc":"2.0""#), "{sent}");
    }

    #[test]
    fn a_notification_arriving_mid_call_does_not_become_the_result() {
        // The bug this whole layer exists to prevent: somebody joins while a
        // backup is asking the server to save, and the join event is returned
        // as the save's answer.
        let mut client = Client::new(Scripted::new(&[
            r#"{"jsonrpc":"2.0","method":"minecraft:notification/players/joined","params":{"player":{"name":"jeb_"}}}"#,
            r#"{"jsonrpc":"2.0","id":1,"result":{"saving":true}}"#,
        ]));

        let result: serde_json::Value = client
            .call("minecraft:server/save", vec![serde_json::json!(true)])
            .unwrap();
        assert_eq!(result["saving"], serde_json::json!(true));
    }

    #[test]
    fn a_stale_reply_to_another_request_is_stepped_over() {
        let mut client = Client::new(Scripted::new(&[
            r#"{"jsonrpc":"2.0","id":7,"result":"stale"}"#,
            r#"{"jsonrpc":"2.0","id":1,"result":"mine"}"#,
        ]));
        let result: String = client.call("minecraft:server/status", vec![]).unwrap();
        assert_eq!(result, "mine");
    }

    #[test]
    fn an_error_reply_carries_its_code_and_message() {
        let mut client = Client::new(Scripted::new(&[
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"Invalid params"}}"#,
        ]));
        let error = client
            .call_raw("minecraft:players/kick", vec![])
            .expect_err("an error reply");
        assert!(error.to_string().contains("Invalid params"), "{error}");
        assert!(error.to_string().contains("-32602"), "{error}");
    }

    #[test]
    fn an_unimplemented_method_is_an_operator_legible_config_error() {
        // The protocol gained methods over time. A server that predates one
        // must produce "your server is too old", not a stack of JSON.
        let mut client = Client::new(Scripted::new(&[
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#,
        ]));
        let error = client
            .call_raw("minecraft:gamerules/", vec![])
            .expect_err("method not found");
        assert_eq!(error.exit_code(), 78, "operator-fixable, not a crash");
        assert!(Client::<Scripted>::is_unsupported(&error), "{error}");
    }

    #[test]
    fn ids_advance_so_two_calls_cannot_answer_each_other() {
        let mut client = Client::new(Scripted::new(&[
            r#"{"jsonrpc":"2.0","id":1,"result":1}"#,
            r#"{"jsonrpc":"2.0","id":2,"result":2}"#,
        ]));
        let _: u64 = client.call("a", vec![]).unwrap();
        let second: u64 = client.call("b", vec![]).unwrap();
        assert_eq!(second, 2);
        assert!(client.transport.sent.borrow()[1].contains(r#""id":2"#));
    }

    #[test]
    fn an_endless_stream_of_notifications_does_not_block_forever() {
        // Without the cap this is a hang, and a hang here is a shutdown that
        // overruns TimeoutStopSec and takes a SIGKILL mid-chunk-flush.
        let notification =
            r#"{"jsonrpc":"2.0","method":"minecraft:notification/server/status","params":{}}"#;
        let script: Vec<&str> = std::iter::repeat_n(notification, MAX_INTERLEAVED + 1).collect();

        let mut client = Client::new(Scripted::new(&script));
        let error = client.call_raw("minecraft:players/", vec![]).unwrap_err();
        assert!(error.to_string().contains("no reply"), "{error}");
    }
}
