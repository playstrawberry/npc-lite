# NPC Lite

NPCs for Play Strawberry servers: player-shaped NPCs with any skin, and NPCs of any other entity
type, with a name, glow, turning to the nearest player, click actions and menus. Players need no
mod.

An NPC is never a real server entity: nearby players are sent it as packets, so it costs the
server almost nothing, never moves, never pushes anyone and is never saved in the world.

## What players see

- **Player NPCs** use the game's mannequin, with the skin you choose. They never show in the tab
  list, `/list` or the player count.
- **Mob NPCs**: any other entity type, for example `villager`, `zombie` or `iron_golem`.
- A **name** above the NPC with `&` colours, optionally one per language (`en`, `fr`, `es`, `tr`).
- Optional **glow**, **baby** form, **sitting**, and **looking** at the nearest player within 10 blocks.
- **Clicks** run the NPC's actions, with a cooldown per player; **menus** open as dialogs with buttons.

## Commands

All under `/npc`. `<name>` is 1 to 32 of `a-z`, `0-9`, `_` and `-`.

| Command | What it does |
| --- | --- |
| `/npc create <name> [type]` | Makes an NPC where you stand; the type defaults to `player` |
| `/npc remove <name>`, `/npc list`, `/npc info <name>` | Removes, lists, shows |
| `/npc movehere <name>` | Moves it to you |
| `/npc name <name> [text]`, `/npc namefor <name> <en\|fr\|es\|tr> [text]` | Its name, or its name in one language |
| `/npc skin <name> [account]` | The skin of a Minecraft account; without one, the default skin |
| `/npc type <name> <type>` | What it looks like |
| `/npc glow\|look\|baby\|sit\|namevisible <name> <true\|false>` | Glow, looking, baby form, sitting, name always shown |
| `/npc range <name> <blocks>` | Shown to players within 8 to 128 blocks (48 by default) |
| `/npc cooldown <name> <ms>` | 0 to 60000 ms between two clicks of one player (500 by default) |
| `/npc permission <name> [node]` | Only players with that permission see it |
| `/npc hide <name> <players>`, `/npc show <name> <players>` | Hides it from players, or shows it again |
| `/npc action <name> add <right\|left\|any> <type> <value>` | Adds a click action |
| `/npc action <name> remove <number>`, `/npc action <name> clear` | Removes one action, or all |
| `/npc menu create <menu> <title>`, `remove`, `list`, `info` | Menus |
| `/npc menu button <menu> <label>`, `/npc menu action <menu> <number> <type> <value>` | Menu buttons and what they do |
| `/npc import` | Reads NPCs from `plugins/data/npc-lite/import.yml` |
| `/npc reload`, `/npc stats` | Reads the NPC file again; shows the plugin's work |

Action types: `message <text>` (`{player}` is the player's name), `teleport <x y z> [yaw pitch]
[world]` or `teleport here`, and `menu <menu>`. `command` and `console` actions are saved but do not
run yet.

## Permissions

| Permission | Default |
| --- | --- |
| `npc-lite:admin` | operators (level 2): every `/npc` command |

## Import

Put a YAML NPC save at `plugins/data/npc-lite/import.yml` and run `/npc import`. Each row becomes
an NPC named `npc-<number>`; running it again changes nothing that exists. The answer lists what was
imported, what was skipped and what could not be carried over (equipment, walking). A file it
cannot read is refused whole.

## Data

NPCs and menus are saved in `plugins/data/npc-lite/npcs.json`, a second after each change and when
the plugin stops. A file that cannot be read is left as it is and no change is accepted until it is
fixed and `/npc reload` reads it. Limits: 1000 NPCs, 16 actions per NPC, 64 menus, 12 buttons per
menu.

## For other plugins

Send a JSON object to `npc-lite` as a plugin message; the answer is a JSON object or an error text.

| Request | Answer |
| --- | --- |
| `{"op":"create","key":"boss","spec":{...}}` | `{"entity_id":N,"key":"boss","replaced":false}` |
| `{"op":"move","key":"boss","place":{...}}`, `{"op":"name",...}` | `{"key":"boss"}` |
| `{"op":"remove","key":"boss"}` | `{"removed":true}` or `false` |
| `{"op":"get","key":"boss"}`, `{"op":"list"}` | the NPC, or the keys |
| `{"op":"is-npc","entity_id":N}` | `{"key":"boss","npc":true,"owner":"my-plugin"}` or `{"npc":false}` |
| `{"op":"click","entity_id":N,"player":"<uuid>","click":"right"}` | `{"accepted":true,...}` or `{"accepted":false}` |

- Keys belong to the plugin that sends them; the same key replaces, never duplicates. One plugin
  holds at most 500 NPCs.
- `spec`: `place` (required: `world`, `x`, `y`, `z`, `yaw`, `pitch`), `type`, `name` (text or one
  per language), `name_visible`, `skin` (`{"value","signature"}` or `{"account":"Name"}`), `glow`,
  `look`, `baby`, `sit`, `persist`, `range`, `cooldown_ms`, `permission`.
- Clicks come by asking: listen for clicks on unknown entities yourself and send `click`; npc-lite
  never calls back into your plugin.

## Languages

English, French, Spanish and Turkish, chosen by each player's game language (`lang/`).

## Install on a Play Strawberry server

1. Add NPC Lite to your server in the Play Strawberry panel.
2. Restart the server. The plugin keeps its NPCs in `plugins/data/npc-lite/`.
3. Make your first NPC with `/npc create guide`.

Play Strawberry servers load only plugins signed by Play Strawberry, so a file you build yourself
from this source does not load on them.

## Build from source

You need Rust with the `wasm32-wasip2` target and the Strawberry plugin SDK
(`strawberry-plugin-api`), which Play Strawberry gives to its plugin developers. Put the SDK in a
folder named `strawberry-plugin-api` next to this one, or change its path in `Cargo.toml`.

```text
cargo test
cargo build --release --target wasm32-wasip2
```

The plugin file is `target/wasm32-wasip2/release/npc_lite.wasm`.

## License

Copyright Strawberry Interactive LLC. The license text is not published yet; until it is, all
rights are reserved.
