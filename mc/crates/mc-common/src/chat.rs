//! Messages sent to players over the server console.

/// Build the command that broadcasts `msg` to every player with no sender
/// prefix.
///
/// NOT `say`. The server renders `say <msg>` as `[<sender>] <msg>`, and for a
/// command arriving over RCON that sender is literally "Rcon", so players see
/// `[Rcon] [Server] …`. `tellraw` writes a raw chat component with no
/// attribution.
///
/// Trade-off worth knowing: `say` is echoed to the server console and so into
/// the journal, while `tellraw` is not. Callers write their own line to stderr,
/// which is where the journal copy comes from.
///
/// THE TEXT IS ESCAPED, NOT TRUSTED. Every message today is an internal
/// literal, but this string is interpolated into a JSON document the server
/// parses and acts on: a bare `"` would close the component early and leave the
/// rest to be read as further JSON.
pub fn say(msg: &str) -> String {
    let mut escaped = String::with_capacity(msg.len() + 2);
    for ch in msg.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            // A chat component is a single line; a raw newline is invalid JSON.
            '\n' | '\r' => escaped.push(' '),
            // The remaining C0 controls would also be invalid unescaped. They
            // cannot appear in an internal literal, but this function's whole
            // job is to be safe for text that did not come from us.
            c if (c as u32) < 0x20 => escaped.push_str(&format!("\\u{:04x}", c as u32)),
            c => escaped.push(c),
        }
    }
    format!("tellraw @a {{\"text\":\"{escaped}\"}}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_never_a_say_command() {
        // `say` arriving over RCON renders as "[Rcon] …" to every player.
        let out = say("Shutting down in 5 minutes.");
        assert!(out.starts_with("tellraw @a "), "{out}");
        assert!(!out.starts_with("say "));
    }

    #[test]
    fn closes_no_json_it_did_not_open() {
        let out = say(r#"a"b"#);
        assert_eq!(out, r#"tellraw @a {"text":"a\"b"}"#);

        // Backslashes are escaped first, or escaping the quote would double up
        // and leave the component malformed.
        assert_eq!(say(r"a\b"), r#"tellraw @a {"text":"a\\b"}"#);
        assert_eq!(say(r#"\""#), r#"tellraw @a {"text":"\\\""}"#);
    }

    #[test]
    fn flattens_newlines_and_control_characters() {
        assert_eq!(say("a\nb\r\nc"), r#"tellraw @a {"text":"a b  c"}"#);
        // A raw tab is invalid inside a JSON string too, so it is escaped
        // rather than flattened — the component still renders as a tab.
        assert_eq!(say("a\tb"), r#"tellraw @a {"text":"a\u0009b"}"#);
    }

    #[test]
    fn an_injected_component_stays_text() {
        // The shape an attacker would try: close the string, close the object,
        // and append a second command's worth of JSON.
        let out = say(r#"x"},{"text":"pwned"#);
        assert!(out.ends_with(r#"pwned"}"#), "{out}");
        assert_eq!(out.matches("tellraw").count(), 1);
        // Exactly one unescaped closing brace pair: the one we wrote.
        assert_eq!(out.matches(r#"{"text":""#).count(), 1);
    }
}
