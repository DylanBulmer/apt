//! Reading a player count out of a `list` reply.
//!
//! The RCON-specific half of counting players: the wire format is a line of
//! prose, and every fork words it differently. What to DO with the answer is
//! policy shared with every other console — see [`mc_console::countdown`].

use mc_console::PlayerCount;

/// Parse the reply to `list`.
///
/// Forks and mods word this differently, and each shape below has been seen in
/// the wild. Anything matching none of them is `Unknown`, never zero.
pub fn parse(reply: &str) -> PlayerCount {
    // Strip Minecraft's §x colour codes (some forks colourise the reply), then
    // flatten whitespace and fold case. Done bytewise and ASCII-only so the
    // result does not depend on the caller's locale.
    let mut normalised = String::with_capacity(reply.len());
    let mut chars = reply.chars();
    while let Some(c) = chars.next() {
        if c == '§' {
            chars.next(); // and the code that follows it
            continue;
        }
        normalised.push(match c {
            '\n' | '\r' | '\t' => ' ',
            other => other.to_ascii_lowercase(),
        });
    }

    let words: Vec<&str> = normalised.split_whitespace().collect();

    // "There are 3 of a max of 20 players online: a, b, c"  (vanilla, paper)
    // "There are 3 out of maximum 20 players online."       (spigot, bukkit)
    //
    // A MATCH ON THE SHAPE IS DECISIVE, even when the number in it does not
    // parse. Falling through to the patterns below would let
    // "There are 99999999999 of a max of 20 players online" match the *max*
    // and report 20 players on a server whose real count is unknown — the
    // wrong answer in the dangerous direction, since a wrong non-zero count is
    // still treated as "warn", but a wrong zero would not be.
    if let Some(index) = words.iter().position(|w| *w == "are") {
        return match words.get(index + 1).map(|w| parse_count(w)) {
            Some(Ok(n)) => PlayerCount::Online(n),
            _ => PlayerCount::Unknown,
        };
    }

    // "There are 3/20 players online"  /  "3/20 players online"
    for word in &words {
        if let Some((left, right)) = word.split_once('/')
            && let (Ok(n), Ok(_)) = (parse_count(left), parse_count(right))
        {
            return PlayerCount::Online(n);
        }
    }

    // "3 players online"
    for (i, word) in words.iter().enumerate() {
        if word.starts_with("player")
            && let Some(previous) = i.checked_sub(1).and_then(|j| words.get(j))
            && let Ok(n) = parse_count(previous)
        {
            return PlayerCount::Online(n);
        }
    }

    PlayerCount::Unknown
}

/// Parse a count, refusing anything implausible.
///
/// Leading zeros are decimal, not octal — a count of `08` must be 8. Rust
/// parses it that way already; the shell version needed `10#` to say so.
fn parse_count(raw: &str) -> Result<u32, ()> {
    let digits: String = raw.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 6 {
        return Err(());
    }
    digits.parse().map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_dialect_seen_in_the_wild() {
        let cases = [
            ("There are 3 of a max of 20 players online: a, b, c", 3),
            ("There are 3 out of maximum 20 players online.", 3),
            ("There are 3/20 players online", 3),
            ("3/20 players online", 3),
            ("3 players online", 3),
            ("There are 0 of a max of 20 players online:", 0),
            ("1 player online", 1),
        ];
        for (reply, expected) in cases {
            assert_eq!(parse(reply), PlayerCount::Online(expected), "{reply:?}");
        }
    }

    #[test]
    fn strips_the_colour_codes_some_forks_send() {
        assert_eq!(
            parse("§aThere are §f7§a of a max of §f20§a players online"),
            PlayerCount::Online(7)
        );
    }

    #[test]
    fn a_zero_padded_count_is_decimal_not_octal() {
        assert_eq!(
            parse("There are 08 of a max of 20 players online"),
            PlayerCount::Online(8)
        );
    }

    #[test]
    fn anything_unrecognised_is_unknown_and_never_zero() {
        // The distinction decides whether a populated server gets five minutes
        // of warning or none.
        for reply in [
            "",
            "   ",
            "Unknown command",
            "§c§lERROR",
            "there are many players",
            "players online",
            "\u{fffd}\u{fffd}\u{fffd}",
        ] {
            assert_eq!(parse(reply), PlayerCount::Unknown, "{reply:?}");
        }
    }

    #[test]
    fn only_a_counted_zero_is_provably_empty() {
        assert!(PlayerCount::Online(0).provably_empty());
        assert!(!PlayerCount::Online(1).provably_empty());
        assert!(
            !PlayerCount::Unknown.provably_empty(),
            "an uncounted server must take the warning path"
        );
    }

    #[test]
    fn an_absurd_count_is_unknown_rather_than_believed() {
        assert_eq!(
            parse("There are 99999999999 of a max of 20 players online"),
            PlayerCount::Unknown
        );
    }
}
