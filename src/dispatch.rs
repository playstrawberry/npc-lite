//! Running the command of an NPC's `command` or `console` action.
//!
//! The plugin API this plugin builds against has no way for a plugin to run a command outside one
//! of its own commands: `server.execute-command` needs a command sender, and a plugin gets one
//! only as the sender of its own command. Until the SDK can run a command for a plugin (as the
//! console, or as a player), this module answers [`Unsupported`], and it is the one place that
//! changes when it can.

use strawberry_plugin_api::Player;

/// Who runs a command.
pub enum Runner<'a> {
    /// The player who clicked, with his own permissions.
    Player(&'a Player),
    /// The server console.
    Console,
}

/// This server cannot run commands for plugins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unsupported;

/// Runs `command` (without its `/`).
pub fn run(runner: Runner<'_>, command: &str) -> Result<(), Unsupported> {
    let _ = (runner, command);
    Err(Unsupported)
}
