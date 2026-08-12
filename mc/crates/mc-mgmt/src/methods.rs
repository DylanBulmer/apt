//! The subset of the protocol mc uses, as typed calls.
//!
//! Two groups, and the distinction matters when the protocol next changes
//! version:
//!
//! * **Console parity** — `players`, `system_message`, `save`, `autosave`,
//!   `stop`, `status`. Everything the elected console must do, and all of it
//!   present since the protocol was introduced in 25w35a. None of it is
//!   touched by the 2.0.0 break, which was confined to game rule value types.
//! * **Moderation** — allowlist, bans, IP bans and operators. New capability
//!   mc has never had over RCON, also stable across the version history.
//!
//! Game rules and the ~20 server settings are deliberately absent: game rules
//! are exactly where the protocol broke compatibility, and adding them means
//! branching on a protocol version rather than calling a method.

use serde::{Deserialize, Serialize};

use mc_common::error::Result;
use mc_console::PlayerCount;

use crate::rpc::{Client, Transport};

/// A player, as the protocol identifies one: at least one of the two fields.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Player {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Player {
    pub fn named(name: &str) -> Self {
        Self {
            id: None,
            name: Some(name.to_string()),
        }
    }

    /// What to print. The protocol guarantees one of the two, not which.
    pub fn label(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.id.clone())
            .unwrap_or_else(|| "<unidentified>".to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Operator {
    pub player: Player,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_level: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bypasses_player_limit: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserBan {
    pub player: Player,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IpBan {
    pub ip: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
}

/// A chat component. `literal` is all mc ever sends.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Message {
    pub literal: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Version {
    pub name: String,
    pub protocol: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerState {
    #[serde(default)]
    pub started: bool,
    #[serde(default)]
    pub players: Vec<Player>,
    pub version: Option<Version>,
}

// ── console parity ─────────────────────────────────────────────────────────

/// Who is online.
///
/// An array, not a sentence. RCON's `list` had to be parsed out of prose that
/// every fork words differently, and anything unrecognised became
/// `PlayerCount::Unknown`; here a successful call is a definite answer.
pub fn players<T: Transport>(client: &mut Client<T>) -> Result<Vec<Player>> {
    client.call("minecraft:players/", vec![])
}

/// The count, in the form the shared countdown policy consumes.
///
/// A failure is `Unknown` rather than an error: not being able to count is
/// exactly the case the countdown treats as "assume somebody is online", and
/// a shutdown must not abort because the question could not be answered.
pub fn player_count<T: Transport>(client: &mut Client<T>) -> PlayerCount {
    match players(client) {
        Ok(players) => PlayerCount::Online(players.len() as u32),
        Err(e) => {
            mc_common::ui::warn(format!("could not count players: {e}"));
            PlayerCount::Unknown
        }
    }
}

/// Broadcast to everyone.
///
/// `overlay: false` puts it in chat rather than the action bar, which is where
/// a shutdown warning has to be: the action bar fades.
pub fn say<T: Transport>(client: &mut Client<T>, text: &str) -> Result<()> {
    let message = serde_json::json!({
        "message": Message { literal: text.to_string() },
        "overlay": false,
    });
    let _: serde_json::Value = client.call("minecraft:server/system_message", vec![message])?;
    Ok(())
}

/// Write the world to disk. `flush` waits for it rather than scheduling it.
pub fn save<T: Transport>(client: &mut Client<T>, flush: bool) -> Result<()> {
    let _: serde_json::Value =
        client.call("minecraft:server/save", vec![serde_json::json!(flush)])?;
    Ok(())
}

/// Turn periodic autosave on or off — the protocol's save-off/save-on.
pub fn set_autosave<T: Transport>(client: &mut Client<T>, enable: bool) -> Result<()> {
    let _: serde_json::Value = client.call(
        "minecraft:serversettings/autosave/set",
        vec![serde_json::json!(enable)],
    )?;
    Ok(())
}

/// Ask the server to stop itself.
pub fn stop<T: Transport>(client: &mut Client<T>) -> Result<()> {
    let _: serde_json::Value = client.call("minecraft:server/stop", vec![])?;
    Ok(())
}

pub fn status<T: Transport>(client: &mut Client<T>) -> Result<ServerState> {
    client.call("minecraft:server/status", vec![])
}

// ── moderation ─────────────────────────────────────────────────────────────

pub fn allowlist<T: Transport>(client: &mut Client<T>) -> Result<Vec<Player>> {
    client.call("minecraft:allowlist/", vec![])
}

pub fn allowlist_add<T: Transport>(
    client: &mut Client<T>,
    players: &[Player],
) -> Result<Vec<Player>> {
    client.call("minecraft:allowlist/add", vec![serde_json::json!(players)])
}

pub fn allowlist_remove<T: Transport>(
    client: &mut Client<T>,
    players: &[Player],
) -> Result<Vec<Player>> {
    client.call(
        "minecraft:allowlist/remove",
        vec![serde_json::json!(players)],
    )
}

pub fn bans<T: Transport>(client: &mut Client<T>) -> Result<Vec<UserBan>> {
    client.call("minecraft:bans/", vec![])
}

pub fn ban_add<T: Transport>(client: &mut Client<T>, bans: &[UserBan]) -> Result<Vec<UserBan>> {
    client.call("minecraft:bans/add", vec![serde_json::json!(bans)])
}

pub fn ban_remove<T: Transport>(
    client: &mut Client<T>,
    players: &[Player],
) -> Result<Vec<UserBan>> {
    client.call("minecraft:bans/remove", vec![serde_json::json!(players)])
}

pub fn ip_bans<T: Transport>(client: &mut Client<T>) -> Result<Vec<IpBan>> {
    client.call("minecraft:ip_bans/", vec![])
}

pub fn ip_ban_add<T: Transport>(client: &mut Client<T>, bans: &[IpBan]) -> Result<Vec<IpBan>> {
    client.call("minecraft:ip_bans/add", vec![serde_json::json!(bans)])
}

pub fn ip_ban_remove<T: Transport>(client: &mut Client<T>, ips: &[String]) -> Result<Vec<IpBan>> {
    client.call("minecraft:ip_bans/remove", vec![serde_json::json!(ips)])
}

pub fn operators<T: Transport>(client: &mut Client<T>) -> Result<Vec<Operator>> {
    client.call("minecraft:operators/", vec![])
}

pub fn operator_add<T: Transport>(
    client: &mut Client<T>,
    operators: &[Operator],
) -> Result<Vec<Operator>> {
    client.call(
        "minecraft:operators/add",
        vec![serde_json::json!(operators)],
    )
}

pub fn operator_remove<T: Transport>(
    client: &mut Client<T>,
    players: &[Player],
) -> Result<Vec<Operator>> {
    client.call(
        "minecraft:operators/remove",
        vec![serde_json::json!(players)],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::fake::Scripted;

    fn client(replies: &[&str]) -> Client<Scripted> {
        Client::new(Scripted::new(replies))
    }

    #[test]
    fn the_player_count_is_the_length_of_a_list_not_a_parsed_sentence() {
        let mut client = client(&[
            r#"{"jsonrpc":"2.0","id":1,"result":[{"name":"jeb_"},{"name":"dinnerbone"}]}"#,
        ]);
        assert_eq!(player_count(&mut client), PlayerCount::Online(2));
    }

    #[test]
    fn an_empty_server_is_provably_empty() {
        // The distinction the countdown turns on: this is what lets a stop
        // skip five minutes of warnings nobody is there to read.
        let mut client = client(&[r#"{"jsonrpc":"2.0","id":1,"result":[]}"#]);
        let count = player_count(&mut client);
        assert_eq!(count, PlayerCount::Online(0));
        assert!(count.provably_empty());
    }

    #[test]
    fn a_failed_count_is_unknown_and_never_zero() {
        // Unknown means "warn anyway". Reporting zero here would disconnect a
        // populated server with no warning the first time the socket dropped.
        let mut client = client(&[r#"{"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"x"}}"#]);
        let count = player_count(&mut client);
        assert_eq!(count, PlayerCount::Unknown);
        assert!(!count.provably_empty());
    }

    #[test]
    fn a_broadcast_goes_to_chat_rather_than_the_fading_overlay() {
        let mut client = client(&[r#"{"jsonrpc":"2.0","id":1,"result":{"sent":true}}"#]);
        say(&mut client, "[Server] Shutting down in 5 minutes.").unwrap();

        let sent = &client_sent(&client)[0];
        assert!(sent.contains(r#""overlay":false"#), "{sent}");
        assert!(sent.contains("Shutting down in 5 minutes."), "{sent}");
    }

    #[test]
    fn a_players_name_is_sent_as_data_not_interpolated_into_a_command() {
        // The structural win over RCON: there is no command string for a name
        // to break out of, so a player called `" §k` is just a string.
        let mut client = client(&[r#"{"jsonrpc":"2.0","id":1,"result":[]}"#]);
        let hostile = Player::named(r#"" §k"#);
        allowlist_add(&mut client, std::slice::from_ref(&hostile)).unwrap();

        let sent = &client_sent(&client)[0];
        let request: serde_json::Value = serde_json::from_str(sent).unwrap();
        assert_eq!(
            request["params"][0][0]["name"],
            serde_json::json!(r#"" §k"#)
        );
    }

    #[test]
    fn a_player_without_a_name_still_has_something_to_print() {
        let player = Player {
            id: Some("853c80ef-3c37-49fd-aa49-938b674adae6".to_string()),
            name: None,
        };
        assert_eq!(player.label(), "853c80ef-3c37-49fd-aa49-938b674adae6");
    }

    #[test]
    fn an_omitted_field_is_left_out_rather_than_sent_as_null() {
        // `Player` requires at least one of id/name, and a null would be a
        // third state the server has to reject.
        let json = serde_json::to_string(&Player::named("jeb_")).unwrap();
        assert_eq!(json, r#"{"name":"jeb_"}"#);
    }

    #[test]
    fn autosave_maps_onto_the_settings_method_rather_than_a_command() {
        let mut client = client(&[r#"{"jsonrpc":"2.0","id":1,"result":{"enabled":false}}"#]);
        set_autosave(&mut client, false).unwrap();
        let sent = &client_sent(&client)[0];
        assert!(
            sent.contains("minecraft:serversettings/autosave/set"),
            "{sent}"
        );
        assert!(sent.contains(r#""params":[false]"#), "{sent}");
    }

    #[test]
    fn status_carries_the_version_the_probe_gates_on() {
        let mut client = client(&[
            r#"{"jsonrpc":"2.0","id":1,"result":{"started":true,"players":[],"version":{"name":"1.21.9","protocol":775}}}"#,
        ]);
        let state = status(&mut client).unwrap();
        assert!(state.started);
        assert_eq!(state.version.unwrap().name, "1.21.9");
    }

    fn client_sent(client: &Client<Scripted>) -> Vec<String> {
        client.transport.sent.borrow().clone()
    }
}
