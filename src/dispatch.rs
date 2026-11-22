//! Running the command of an NPC's `command` or `console` action.
//!
//! A security boundary, so every rule is a pure function here and the one call that runs a line
//! ([`execute`]) is the last step. What runs is only what `/npc` stored, with `{player}` filled in
//! by the clicking player's own checked name. Any doubt refuses (fails closed): [`prepare`] answers
//! a [`Refused`] and nothing runs.
//!
//! The shared lists are the ones `user_interface` and `miniscript` read (`command_rules`): the
//! platform's command names never run through an action, and the console never runs a command that
//! hands the server away or runs other commands.

use crate::model::MAX_COMMAND;

/// The shortest time between two commands the actions of one player run (the same gap as
/// `user_interface`'s buttons): a client that clicks as fast as it can still runs a few a second.
pub const COMMAND_GAP_MS: u64 = 250;

/// The longest line that runs, `{player}` filled in. A stored command is at most `MAX_COMMAND`
/// characters; each `{player}` (8 characters) becomes at most 16.
pub const MAX_RUN: usize = MAX_COMMAND * 2;

/// The text a stored command holds in place of the clicking player's name.
const PLAYER: &str = "{player}";

/// Who runs a command, with the line to run (without its `/`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Run {
    /// The player who clicked, with his own permissions: the server answers a command he may not
    /// use exactly as if he had typed it.
    AsPlayer(String),
    /// The server console, with every right.
    FromConsole(String),
}

impl Run {
    /// The line that runs.
    #[must_use]
    pub fn line(&self) -> &str {
        match self {
            Self::AsPlayer(line) | Self::FromConsole(line) => line,
        }
    }
}

/// Why a command did not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Nothing to run.
    Empty,
    /// A line break or another control character.
    Control,
    /// Longer than the bound.
    TooLong,
    /// `{player}` is used and the player's name is not one a Minecraft account has.
    BadName,
    /// The first word is not written out in the stored command (a `{player}` there would let a
    /// player's name be the command).
    NoPlainName,
    /// A command of the platform (`/marketplace` ...): never run through an action.
    Platform,
    /// A command the console never runs for anyone's word (`op`, `stop`, `execute` ...).
    ForbiddenFromConsole,
}

/// Whether `name` is a Minecraft Java account name: 3 to 16 of `A-Z a-z 0-9 _`.
#[must_use]
pub fn valid_player_name(name: &str) -> bool {
    (3..=16).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Whether a player whose action last ran a command at `last` may run one at `now`. A clock that
/// went back does not open the gate.
#[must_use]
pub const fn may_run(last: Option<u64>, now: u64) -> bool {
    match last {
        None => true,
        Some(last) => now >= last && now - last >= COMMAND_GAP_MS,
    }
}

/// Whether a sender may store or change an action. A `console` action runs with every right, so
/// it needs the console right (`npc-lite:admin.console`, the console and operators of level 4 by
/// default) on top of `/npc`'s own: without it a holder of `npc-lite:admin` (operators of level 2
/// by default) could reach every right through an NPC. Any other action needs only `/npc`.
#[must_use]
pub const fn may_store(console: bool, console_right: bool) -> bool {
    !console || console_right
}

/// The checks a stored command passes before it is kept and again before it runs, without a
/// player: one line, not empty, at most `MAX_COMMAND` characters, a first word written out in
/// plain letters (no `{player}`, no other brace), no platform command, and from the console none
/// of the commands the console never runs.
pub fn check_stored(console: bool, command: &str) -> Result<(), Refused> {
    let command = command.trim();
    let command = command.strip_prefix('/').unwrap_or(command).trim_start();
    if command.is_empty() {
        return Err(Refused::Empty);
    }
    if command.chars().any(char::is_control) {
        return Err(Refused::Control);
    }
    if command.chars().count() > MAX_COMMAND {
        return Err(Refused::TooLong);
    }
    let Some(root) = command_rules::root(command) else {
        return Err(Refused::Empty);
    };
    // The command's name is written out in the stored text: a placeholder there (a player may be
    // called `stop`) or anything that only looks like a name is not one.
    if !root
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b))
    {
        return Err(Refused::NoPlainName);
    }
    judge_root(console, &root)
}

fn judge_root(console: bool, root: &str) -> Result<(), Refused> {
    if command_rules::platform(root) {
        return Err(Refused::Platform);
    }
    if console && command_rules::forbidden_from_console(root) {
        return Err(Refused::ForbiddenFromConsole);
    }
    Ok(())
}

/// The line one click runs: `command` as stored, `{player}` filled in with `player` (only when the
/// command uses it, and only when `player` is a valid account name), then the first word judged
/// again on the filled line. Everything but the name comes from the store; nothing a player sends
/// is part of it.
pub fn prepare(console: bool, command: &str, player: &str) -> Result<Run, Refused> {
    check_stored(console, command)?;
    let command = command.trim();
    let command = command.strip_prefix('/').unwrap_or(command).trim_start();
    let line = if command.contains(PLAYER) {
        if !valid_player_name(player) {
            return Err(Refused::BadName);
        }
        command.replace(PLAYER, player)
    } else {
        command.to_owned()
    };
    if line.chars().count() > MAX_RUN {
        return Err(Refused::TooLong);
    }
    if line.chars().any(char::is_control) {
        return Err(Refused::Control);
    }
    // The filled line is judged on its first word again, so the name that was put in cannot have
    // changed which command it is.
    let Some(root) = command_rules::root(&line) else {
        return Err(Refused::Empty);
    };
    judge_root(console, &root)?;
    Ok(if console {
        Run::FromConsole(line)
    } else {
        Run::AsPlayer(line)
    })
}

/// Runs the line. Called on a later tick than the click, and only for a player who is still there:
/// the one who runs it is told what the command answers, an unknown command too.
pub fn execute(
    server: &strawberry_plugin_api::Server,
    player: &strawberry_plugin_api::Player,
    run: &Run,
) {
    let _ = match run {
        Run::AsPlayer(line) => player.perform_command(line),
        Run::FromConsole(line) => {
            server.dispatch_command(line, strawberry_plugin_api::server::CommandSender::Console)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_console_action_needs_the_console_right_and_other_actions_do_not() {
        assert!(may_store(true, true));
        assert!(!may_store(true, false));
        assert!(may_store(false, false));
        assert!(may_store(false, true));
    }

    #[test]
    fn a_command_action_runs_as_the_player() {
        assert_eq!(
            prepare(false, "warp spawn", "Steve"),
            Ok(Run::AsPlayer("warp spawn".into()))
        );
        assert_eq!(
            prepare(false, "/warp spawn", "Steve"),
            Ok(Run::AsPlayer("warp spawn".into()))
        );
    }

    #[test]
    fn a_console_action_runs_from_the_console() {
        assert_eq!(
            prepare(true, "say hello", "Steve"),
            Ok(Run::FromConsole("say hello".into()))
        );
    }

    #[test]
    fn player_is_filled_with_a_valid_name_and_refused_with_another() {
        assert_eq!(
            prepare(true, "give {player} bread 2", "Steve_01"),
            Ok(Run::FromConsole("give Steve_01 bread 2".into()))
        );
        assert_eq!(
            prepare(false, "msg {player} hi {player}", "Alex"),
            Ok(Run::AsPlayer("msg Alex hi Alex".into()))
        );
        for bad in [
            "ab",
            "a_name_that_is_far_too_long",
            "Al ex",
            ".Bedrock",
            "x;op",
            "a\nb",
            "",
            "Név",
            "{player}",
        ] {
            assert_eq!(
                prepare(true, "give {player} bread", bad),
                Err(Refused::BadName),
                "{bad:?}"
            );
        }
        // A command that does not use `{player}` does not need a valid name.
        assert!(prepare(false, "spawn", ".Bedrock").is_ok());
    }

    #[test]
    fn a_name_cannot_be_the_command() {
        assert_eq!(
            prepare(true, "{player} now", "stop"),
            Err(Refused::NoPlainName)
        );
        assert_eq!(
            prepare(false, "{player}", "spawn"),
            Err(Refused::NoPlainName)
        );
    }

    #[test]
    fn platform_commands_never_run() {
        for command in [
            "/marketplace",
            "marketplace",
            "/strawberry:marketplace",
            "/MarketPlace",
            "  /MARKETPLACE buy x",
            "minecraft:marketplace",
            "store",
            "/ marketplace",
        ] {
            for console in [false, true] {
                assert_eq!(
                    prepare(console, command, "Steve"),
                    Err(Refused::Platform),
                    "{command:?} console={console}"
                );
            }
        }
    }

    #[test]
    fn the_console_never_runs_what_hands_the_server_away() {
        for command in ["op {player}", "stop", "execute run say hi", "other:deop x"] {
            assert_eq!(
                prepare(true, command, "Steve"),
                Err(Refused::ForbiddenFromConsole),
                "{command}"
            );
        }
        // As the player they are the server's own permission check.
        assert!(prepare(false, "op Steve", "Steve").is_ok());
    }

    #[test]
    fn a_doubtful_command_is_refused() {
        assert_eq!(prepare(false, "", "Steve"), Err(Refused::Empty));
        assert_eq!(prepare(false, "  /  ", "Steve"), Err(Refused::Empty));
        assert_eq!(
            prepare(false, "say a\nop b", "Steve"),
            Err(Refused::Control)
        );
        assert_eq!(prepare(true, "say a\rb", "Steve"), Err(Refused::Control));
        assert_eq!(prepare(true, "say a\u{0}b", "Steve"), Err(Refused::Control));
        let long = format!("say {}", "x".repeat(MAX_COMMAND));
        assert_eq!(prepare(true, &long, "Steve"), Err(Refused::TooLong));
        let edge = format!("say {}", "x".repeat(MAX_COMMAND - 4));
        assert!(prepare(true, &edge, "Steve").is_ok());
    }

    #[test]
    fn a_stored_command_is_checked_without_a_player() {
        assert_eq!(check_stored(false, "marketplace"), Err(Refused::Platform));
        assert_eq!(
            check_stored(true, "stop"),
            Err(Refused::ForbiddenFromConsole)
        );
        assert_eq!(check_stored(false, "{player} x"), Err(Refused::NoPlainName));
        assert_eq!(check_stored(false, "give {player} bread"), Ok(()));
    }

    #[test]
    fn a_second_click_within_the_gap_runs_nothing() {
        assert!(may_run(None, 0));
        assert!(!may_run(Some(1000), 1000));
        assert!(!may_run(Some(1000), 1000 + COMMAND_GAP_MS - 1));
        assert!(may_run(Some(1000), 1000 + COMMAND_GAP_MS));
        // The clock went back: closed.
        assert!(!may_run(Some(1000), 999));
    }

    #[test]
    fn valid_names_are_three_to_sixteen_word_characters() {
        for ok in ["abc", "Steve", "a_b_c_d_e_f_g_h_", "A1_"] {
            assert!(valid_player_name(ok), "{ok}");
        }
        for bad in ["", "ab", "abcdefghijklmnopq", "a b", "a-b", "é_é"] {
            assert!(!valid_player_name(bad), "{bad}");
        }
    }
}
