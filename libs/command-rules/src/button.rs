//! A button of a file UI that names a function (`"function": "kit"`): `user_interface` tells
//! `miniscript`, which runs the server owner's `on button kit` blocks for the player who pressed.
//!
//! The message says the function's name, which is fixed text of the owner's file, who pressed, which
//! the server knows, and the button's values: what the button's file hands
//! the function (`"args"`), such as what the player filled in on a dialog. The values are data: the
//! script gets each one as one value of the block's arguments (`on button buy <item> <n: number>`),
//! checked against its type like a command's argument, and never as code. `miniscript` takes the
//! message from `user_interface` only (the server names the sender), so no other plugin starts an
//! owner's script this way.

/// The plugin that sends the message.
pub const UI_PLUGIN: &str = "user_interface";
/// The plugin that takes it.
pub const SCRIPT_PLUGIN: &str = "miniscript";

/// The longest function name.
pub const MAX_NAME: usize = 32;

/// A function's name: a word a Mini-script can write after `on button` (lower-case letters, digits
/// and `_`, no digit first, at most [`MAX_NAME`] long).
#[must_use]
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.bytes();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == b'_')
        && name.len() <= MAX_NAME
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

/// The most values one press hands a function.
pub const MAX_ARGS: usize = 8;
/// The longest value, in characters.
pub const MAX_ARG: usize = 256;

/// A value a press may carry: at most [`MAX_ARG`] characters on one line (no control character, so
/// no tab and no line break).
#[must_use]
pub fn valid_arg(value: &str) -> bool {
    value.chars().count() <= MAX_ARG && !value.chars().any(char::is_control)
}

const WORD: &str = "button";

/// A press as `miniscript` reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Press<'a> {
    /// The function's name.
    pub name: &'a str,
    /// Who pressed.
    pub player: u128,
    /// The button's values, in the order its file lists them.
    pub args: Vec<&'a str>,
}

/// The message for a press of a button whose function is `name`, by the player `player`, with the
/// button's values. None when a value is not one a press may carry ([`valid_arg`], at most
/// [`MAX_ARGS`]): the sender then runs nothing.
#[must_use]
pub fn message(name: &str, player: u128, args: &[String]) -> Option<Vec<u8>> {
    if !valid_name(name) || args.len() > MAX_ARGS || !args.iter().all(|a| valid_arg(a)) {
        return None;
    }
    let mut out = format!("{WORD}\t{name}\t{player:032x}");
    for a in args {
        out.push('\t');
        out.push_str(a);
    }
    Some(out.into_bytes())
}

/// Reads a message back. None for anything that is not exactly what [`message`] writes.
#[must_use]
pub fn read(message: &[u8]) -> Option<Press<'_>> {
    let text = std::str::from_utf8(message).ok()?;
    let mut parts = text.split('\t');
    let (word, name, player) = (parts.next()?, parts.next()?, parts.next()?);
    if word != WORD || !valid_name(name) {
        return None;
    }
    let args: Vec<&str> = parts.collect();
    if args.len() > MAX_ARGS || !args.iter().all(|a| valid_arg(a)) {
        return None;
    }
    let hex = player.len() == 32
        && player
            .bytes()
            .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'));
    hex.then(|| u128::from_str_radix(player, 16).ok())
        .flatten()
        .map(|player| Press { name, player, args })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_a_word_a_script_can_write() {
        for good in ["kit", "shop_buy", "_x", "a1", &"a".repeat(MAX_NAME)] {
            assert!(valid_name(good), "{good}");
        }
        for bad in [
            "",
            "Kit",
            "1kit",
            "shop-buy",
            "shop buy",
            "kit\t",
            "ki:t",
            "é",
            &"a".repeat(MAX_NAME + 1),
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn a_message_is_read_back_and_nothing_else_is() {
        let id = 0x0123_4567_89ab_cdef_0123_4567_89ab_cdef_u128;
        let press = |name, player, args: Vec<&'static str>| Press { name, player, args };
        let m = message("kit", id, &[]).unwrap();
        assert_eq!(m, b"button\tkit\t0123456789abcdef0123456789abcdef");
        assert_eq!(read(&m), Some(press("kit", id, vec![])));
        assert_eq!(
            read(&message("kit", 0, &[]).unwrap()),
            Some(press("kit", 0, vec![]))
        );
        for bad in [
            &b""[..],
            b"button",
            b"button\tkit",
            b"button\tkit\t0123456789abcdef0123456789abcde",
            b"button\tkit\t0123456789ABCDEF0123456789abcdef",
            b"button\tkit\t+123456789abcdef0123456789abcdef",
            b"button\tKit\t0123456789abcdef0123456789abcdef",
            b"click\tkit\t0123456789abcdef0123456789abcdef",
            b"button\tkit x\t0123456789abcdef0123456789abcdef",
            b"\xff\xfe",
        ] {
            assert_eq!(read(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn values_ride_along_as_data_one_line_each_and_bounded() {
        let id = 7_u128;
        let args = vec![
            "red wool".to_owned(),
            String::new(),
            "{player}; op x".to_owned(),
        ];
        let m = message("buy", id, &args).unwrap();
        let read_back = read(&m).unwrap();
        assert_eq!(read_back.name, "buy");
        assert_eq!(read_back.player, id);
        // Each value comes back as it went, one per place, nothing filled in or split.
        assert_eq!(read_back.args, ["red wool", "", "{player}; op x"]);
        // A value with a tab or a line break could pose as two values: it is never written.
        for bad in ["a\tb", "a\nb", "a\u{0}"] {
            assert_eq!(message("buy", id, &[bad.to_owned()]), None, "{bad:?}");
        }
        assert_eq!(message("buy", id, &["x".repeat(MAX_ARG + 1)]), None);
        assert!(message("buy", id, &["x".repeat(MAX_ARG)]).is_some());
        assert_eq!(message("buy", id, &vec![String::new(); MAX_ARGS + 1]), None);
        assert!(message("buy", id, &vec![String::new(); MAX_ARGS]).is_some());
        assert_eq!(message("Buy", id, &[]), None);
        // The reader holds the same bounds for a message it did not write.
        let head = "button\tbuy\t00000000000000000000000000000007";
        let many = format!("{head}{}", "\tx".repeat(MAX_ARGS + 1));
        assert_eq!(read(many.as_bytes()), None);
        let long = format!("{head}\t{}", "x".repeat(MAX_ARG + 1));
        assert_eq!(read(long.as_bytes()), None);
        assert_eq!(read(format!("{head}\ta\rb").as_bytes()), None);
    }
}
