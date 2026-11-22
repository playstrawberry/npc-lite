//! The command rules every first-party script shares, in one place: the
//! commands that are never run from the console on someone else's word, and the command names that
//! belong to the platform. `miniscript` (a script's `console` action) and `user_interface` (a file
//! button's command) read the same lists, so a rule added here holds for both. `button` is the one
//! message between the two: a pressed button of a file that names a function.

pub mod button;

/// Console commands nothing may run, whatever is declared or allowed: they run other commands (so a
/// list of allowed commands would mean nothing), or they hand the server to someone.
pub const FORBIDDEN_CONSOLE: [&str; 13] = [
    "execute",
    "function",
    "schedule",
    "op",
    "deop",
    "stop",
    "reload",
    "plugin",
    "plugins",
    "miniscript",
    "datapack",
    "sudo",
    "return",
];

/// Command names of the platform: no script takes one as its own command, and no file runs one
/// for a player (a purchase is the player's own typed word).
pub const PLATFORM_COMMANDS: [&str; 3] = ["marketplace", "store", "plugin"];

/// The first word of a command (its root): lower case, without `/` and `minecraft:`.
#[must_use]
pub fn root(command: &str) -> Option<String> {
    let first = command
        .trim_start()
        .trim_start_matches('/')
        .split_whitespace()
        .next()?;
    let first = first.to_ascii_lowercase();
    let root = first.strip_prefix("minecraft:").unwrap_or(&first);
    (!root.is_empty()).then(|| root.to_owned())
}

/// The part of a root a server may resolve it to: the name after its last namespace (`other:op` is
/// `op`), so a namespaced form never passes a list its plain name is on.
fn plain(root: &str) -> &str {
    root.rsplit(':').next().unwrap_or(root)
}

/// Whether the console may never run this root: a forbidden name, in any namespace, or a root with
/// an empty name. Fails closed: whether the server resolves `<namespace>:op` to `op` is not asked.
#[must_use]
pub fn forbidden_from_console(root: &str) -> bool {
    let name = plain(root);
    name.is_empty() || FORBIDDEN_CONSOLE.contains(&root) || FORBIDDEN_CONSOLE.contains(&name)
}

/// Whether this root is a command of the platform, in any namespace.
#[must_use]
pub fn platform(root: &str) -> bool {
    PLATFORM_COMMANDS.contains(&root) || PLATFORM_COMMANDS.contains(&plain(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_ignore_slash_case_and_the_minecraft_namespace() {
        assert_eq!(root("/Minecraft:Say hi").as_deref(), Some("say"));
        assert_eq!(root("  time set day").as_deref(), Some("time"));
        assert_eq!(root("//op x").as_deref(), Some("op"));
        assert_eq!(root("   "), None);
        assert_eq!(root("/"), None);
        // Another namespace stays part of the root: it is not the vanilla command.
        assert_eq!(root("other:op x").as_deref(), Some("other:op"));
    }

    #[test]
    fn the_lists_hold_what_hands_the_server_away_and_the_platform() {
        for cmd in [
            "op", "deop", "stop", "execute", "function", "plugin", "sudo",
        ] {
            assert!(forbidden_from_console(cmd), "{cmd}");
        }
        assert!(!forbidden_from_console("say"));
        for cmd in ["marketplace", "store", "plugin"] {
            assert!(platform(cmd), "{cmd}");
        }
        assert!(!platform("spawn"));
    }

    #[test]
    fn a_namespaced_root_is_judged_by_its_plain_name_too() {
        for root in [
            "other:op",
            "strawberry:stop",
            "a:b:execute",
            "x:function",
            "plugin:sudo",
        ] {
            assert!(forbidden_from_console(root), "{root}");
        }
        for root in ["other:", ":", "a::"] {
            assert!(forbidden_from_console(root), "an empty name: {root}");
        }
        assert!(!forbidden_from_console("other:say"));
        assert!(!forbidden_from_console("operator"));
        assert!(platform("other:marketplace"));
        assert!(platform("x:store"));
        assert!(!platform("other:spawn"));
    }
}
