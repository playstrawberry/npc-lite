//! The NPCs and menus the server owner made, their limits, and the data file (`npcs.json`).
//!
//! Everything read back from the file is untrusted (the server owner and his tools can change
//! it): a file that breaks one rule is refused as a whole, and the caller keeps it untouched.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

use crate::wire;

pub const MAX_NPCS: usize = 1000;
pub const MAX_ACTIONS: usize = 16;
pub const MAX_MENUS: usize = 64;
pub const MAX_BUTTONS: usize = 12;
pub const MAX_HIDDEN: usize = 10_000;
pub const MAX_NAME: usize = 256;
pub const MAX_MESSAGE: usize = 512;
pub const MAX_COMMAND: usize = 256;
pub const MAX_COOLDOWN_MS: u32 = 60_000;
/// The largest data file read (4 MiB).
pub const MAX_FILE: usize = 4 << 20;
/// The largest NPC number: entity ids are [`ENTITY_ID_BASE`] plus the number.
pub const MAX_NUM: u32 = 1_000_000;
/// Packet-only NPCs take entity ids from here up, far above what a server hands out.
pub const ENTITY_ID_BASE: i32 = 0x7A00_0000;
/// The invisible seat of a sitting NPC has this base plus the NPC's number.
pub const SEAT_ID_BASE: i32 = 0x7B00_0000;
/// The entity a sitting NPC rides: it draws nothing and has no box.
pub const SEAT_TYPE: &str = "text_display";
/// Why an NPC that would both sit and lie is refused (a plugin's spec, the data file).
pub const SIT_AND_LIE: &str = "an NPC sits or lies, not both";
/// The languages a name can be given for.
pub const LANGUAGES: [&str; 4] = ["en", "fr", "es", "tr"];
/// One plugin may hold this many NPCs, so it cannot use up the server's limit.
pub const MAX_PER_OWNER: usize = 500;
pub const DEFAULT_COOLDOWN_MS: u32 = 500;
/// An NPC is sent within this many blocks unless its own range says otherwise.
pub const DEFAULT_RANGE: u32 = 48;
pub const MIN_RANGE: u32 = 8;
/// A client sees entities at most this far (view distance 8).
pub const MAX_RANGE: u32 = 128;
/// An NPC is drawn at its normal size unless its scale says otherwise.
pub const DEFAULT_SCALE: f32 = 1.0;
/// The range of the game's `minecraft:scale` attribute.
pub const MIN_SCALE: f32 = 0.0625;
pub const MAX_SCALE: f32 = 16.0;
const FORMAT_VERSION: u64 = 1;
const PLAYER_RIDE_OFFSET: f64 = 0.6;
/// Entity types an NPC cannot be: a player entity needs a tab-list entry (a player NPC is a
/// mannequin), and these others are not drawn as an entity of their own.
const REFUSED_TYPES: [&str; 3] = ["player", "fishing_bobber", "marker"];

/// Where something is.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub world: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
}

/// What an NPC looks like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// Player-shaped, with a skin (a mannequin: never in the tab list).
    Player,
    /// Any other entity type, by its name without `minecraft:`.
    Mob(String),
}

/// Which click runs an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Right,
    Left,
    Any,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// A chat line to the player (`&` colour codes, `{player}` for his name): the line of his
    /// language in `texts` when it has one, else `text`.
    Message {
        text: String,
        texts: BTreeMap<String, String>,
    },
    /// A command the player runs (without the `/`).
    Command(String),
    /// A command the console runs (`{player}` for the player's name).
    Console(String),
    Teleport(Place),
    /// Opens the menu of this name.
    Menu(String),
    /// Opens the server's own `user_interface` file of this id (a screen, page or store with an
    /// `open_command`, `open_key` or `open_item`) for the player, as its command would.
    Ui(String),
}

impl Action {
    /// A message with one text for every language.
    #[must_use]
    pub fn message(text: &str) -> Self {
        Self::Message {
            text: text.to_string(),
            texts: BTreeMap::new(),
        }
    }
}

/// The line of a message a player of `language` reads: his language's, else the message's text.
#[must_use]
pub fn line_for<'a>(text: &'a str, texts: &'a BTreeMap<String, String>, language: &str) -> &'a str {
    texts.get(language).map_or(text, String::as_str)
}

/// A skin: the account it was taken from and its signed textures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skin {
    pub from: String,
    pub value: String,
    pub signature: Option<String>,
}

/// The arms a skin is drawn with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkinModel {
    Wide,
    Slim,
}

/// A skin from the server's resource pack, drawn instead of the profile's (26.3's skin patch):
/// `texture` is `namespace:path` for `assets/<namespace>/textures/<path>.png`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkinPatch {
    pub texture: String,
    pub model: SkinModel,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Npc {
    pub id: String,
    pub num: u32,
    pub uuid: (u64, u64),
    pub place: Place,
    pub kind: Kind,
    /// `&` colour codes; `None` shows no name.
    pub name: Option<String>,
    /// A name per language (`en`, `fr`, `es`, `tr`); a viewer whose language has none reads `name`.
    pub names: BTreeMap<String, String>,
    pub name_visible: bool,
    pub skin: Option<Skin>,
    /// A player NPC drawn, for each player who sees it, in that player's own skin (`skin` is then
    /// unused). Each viewer is sent only his own skin.
    pub viewer_skin: bool,
    /// A player NPC drawn with a texture of the server's resource pack instead of its skin.
    pub skin_patch: Option<SkinPatch>,
    pub glow: bool,
    /// Turns to the nearest player (off unless set: a turning NPC costs packets).
    pub look: bool,
    /// Shown as a baby (only types with a baby flag).
    pub baby: bool,
    /// Sits on an invisible seat of its own.
    pub sit: bool,
    /// Lies on the ground, as a player sleeping without a bed (never with `sit`).
    pub lie: bool,
    /// How large it is drawn, as the game's `minecraft:scale` attribute: 1 is the normal size, and a
    /// larger figure is drawn from farther away (`valid_scale`).
    pub scale: f32,
    /// The plugin that made it (`None`: the server owner, with `/npc`). Only that plugin changes it;
    /// a click on it runs its actions when it has some, else the plugin asks about the click.
    pub owner: Option<String>,
    /// Written to the data file (a plugin's NPC can be kept in memory only).
    pub persist: bool,
    /// Sent within this many blocks.
    pub range: u32,
    pub cooldown_ms: u32,
    /// Only players with this permission see the NPC.
    pub permission: Option<String>,
    /// Players (UUID text) who do not see the NPC.
    pub hidden: BTreeSet<String>,
    pub actions: Vec<(Trigger, Action)>,
}

impl Npc {
    /// The entity id the NPC has on every client.
    #[must_use]
    pub fn entity_id(&self) -> i32 {
        entity_id(self.num)
    }

    /// Its name without the plugin that owns it.
    #[must_use]
    pub fn key(&self) -> &str {
        self.owner
            .as_deref()
            .and_then(|owner| self.id.strip_prefix(owner))
            .and_then(|rest| rest.strip_prefix('.'))
            .unwrap_or(&self.id)
    }

    /// The entity id of its seat.
    #[must_use]
    pub fn seat_id(&self) -> i32 {
        seat_id(self.num)
    }

    /// The UUID of its seat: its own with the low half changed.
    #[must_use]
    pub fn seat_uuid(&self) -> (u64, u64) {
        (self.uuid.0, self.uuid.1 ^ 0x5EA7_5EA7_5EA7_5EA7)
    }

    /// Where its seat is, so that its feet rest at its place: a player shape rides 0.6 blocks below
    /// its vehicle's attachment point, other shapes at it.
    #[must_use]
    pub fn seat_y(&self) -> f64 {
        self.place.y
            + if self.kind == Kind::Player {
                PLAYER_RIDE_OFFSET
            } else {
                0.0
            }
    }

    /// The name a viewer of this language reads.
    #[must_use]
    pub fn name_for(&self, language: &str) -> Option<&str> {
        self.names
            .get(language)
            .or(self.name.as_ref())
            .map(String::as_str)
    }

    /// The protocol id and eye height of what the NPC looks like.
    #[must_use]
    pub fn entity_type(&self) -> (i32, f32) {
        match &self.kind {
            Kind::Player => (wire::MANNEQUIN, 1.62),
            Kind::Mob(name) => wire::entity_type(name).unwrap_or((wire::MANNEQUIN, 1.62)),
        }
    }
}

/// The entity id of the NPC with this number.
#[must_use]
pub fn entity_id(num: u32) -> i32 {
    ENTITY_ID_BASE.saturating_add(i32::try_from(num).unwrap_or(i32::MAX))
}

/// The entity id of the seat of the NPC with this number.
#[must_use]
pub fn seat_id(num: u32) -> i32 {
    SEAT_ID_BASE.saturating_add(i32::try_from(num).unwrap_or(i32::MAX))
}

/// The NPC number of an entity id, when it is one.
#[must_use]
pub fn num_of(entity_id: i32) -> Option<u32> {
    let num = u32::try_from(entity_id.checked_sub(ENTITY_ID_BASE)?).ok()?;
    (1..=MAX_NUM).contains(&num).then_some(num)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Button {
    pub label: String,
    pub action: Option<Action>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Menu {
    pub name: String,
    pub title: String,
    pub buttons: Vec<Button>,
}

/// What a plugin asks an NPC of its own to be.
#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    pub kind: Kind,
    pub place: Place,
    pub name: Option<String>,
    pub names: BTreeMap<String, String>,
    pub name_visible: bool,
    pub skin: Option<Skin>,
    /// Each viewer sees the NPC in his own skin.
    pub viewer_skin: bool,
    pub skin_patch: Option<SkinPatch>,
    pub glow: bool,
    pub look: bool,
    pub baby: bool,
    pub sit: bool,
    pub lie: bool,
    pub scale: f32,
    pub persist: bool,
    pub range: u32,
    pub cooldown_ms: u32,
    pub permission: Option<String>,
    /// What a click does; none: the plugin asks about each click itself.
    pub actions: Vec<(Trigger, Action)>,
}

/// What [`Store::put_owned`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Put {
    pub num: u32,
    pub replaced: bool,
    /// The earlier NPC of this key was written to the file.
    pub was_persisted: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Store {
    pub npcs: BTreeMap<String, Npc>,
    pub menus: BTreeMap<String, Menu>,
    /// The number the next NPC gets. Numbers are never reused.
    pub next_num: u32,
}

/// Why an NPC was not made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateError {
    BadId,
    Exists,
    Limit,
}

/// An NPC or menu name: 1 to 32 of a-z, 0-9, `_` and `-`.
#[must_use]
pub fn valid_id(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// The name of a plugin that owns NPCs: what a command can take as one word, so that
/// `<plugin>.<key>` can be typed.
#[must_use]
pub fn valid_owner(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'+'))
}

/// The name in the store of the NPC `key` of the plugin `owner`. Plugins' NPCs never share a name
/// with each other (a key has no `.`) or with an NPC made with `/npc`, which has no `.`.
#[must_use]
pub fn owned_id(owner: &str, key: &str) -> String {
    format!("{owner}.{key}")
}

/// A name per language: only the known languages, each a valid text.
#[must_use]
pub fn valid_names(names: &BTreeMap<String, String>) -> bool {
    valid_texts(names, MAX_NAME)
}

/// Texts per language: only the known languages, each a valid text of at most `max` characters.
#[must_use]
pub fn valid_texts(texts: &BTreeMap<String, String>, max: usize) -> bool {
    texts
        .iter()
        .all(|(language, text)| LANGUAGES.contains(&language.as_str()) && valid_text(text, max))
}

/// A texture of a resource pack as the game names it: `namespace:path` (or `path` for
/// `minecraft:`), at most 256 characters; a namespace of a-z, 0-9, `_`, `-`, `.`, a path of those
/// and `/`.
#[must_use]
pub fn valid_texture(texture: &str) -> bool {
    let (namespace, path) = texture.split_once(':').unwrap_or(("minecraft", texture));
    let allowed = |b: u8, slash: bool| {
        b.is_ascii_lowercase()
            || b.is_ascii_digit()
            || matches!(b, b'_' | b'-' | b'.')
            || (slash && b == b'/')
    };
    texture.len() <= 256
        && !namespace.is_empty()
        && !path.is_empty()
        && namespace.bytes().all(|b| allowed(b, false))
        && path.bytes().all(|b| allowed(b, true))
}

/// The id of a `user_interface` file: `[a-z0-9_.:-]`, 1 to 64 characters, as that plugin names
/// its screens, pages and stores. Never a path.
#[must_use]
pub fn valid_ui_file(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.:-".contains(&b))
}

/// A skin model by its name in the game's data (`wide`, `slim`).
#[must_use]
pub fn parse_model(name: &str) -> Option<SkinModel> {
    match name {
        "wide" => Some(SkinModel::Wide),
        "slim" => Some(SkinModel::Slim),
        _ => None,
    }
}

#[must_use]
pub fn model_name(model: SkinModel) -> &'static str {
    match model {
        SkinModel::Wide => "wide",
        SkinModel::Slim => "slim",
    }
}

/// A Minecraft account name: 1 to 16 of letters, digits and `_`.
#[must_use]
pub fn valid_account(name: &str) -> bool {
    (1..=16).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// A permission node: 1 to 128 of a-z, 0-9, `.`, `_`, `-` and `:`.
#[must_use]
pub fn valid_node(node: &str) -> bool {
    (1..=128).contains(&node.len())
        && node.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-' | b':')
        })
}

/// A world name as the server writes it.
#[must_use]
pub fn valid_world(name: &str) -> bool {
    (1..=64).contains(&name.chars().count()) && !name.chars().any(char::is_control)
}

/// A line of text: at most `max` characters, no control characters.
#[must_use]
pub fn valid_text(text: &str, max: usize) -> bool {
    !text.is_empty() && text.chars().count() <= max && !text.chars().any(char::is_control)
}

fn valid_base64(text: &str, max: usize) -> bool {
    !text.is_empty()
        && text.len() <= max
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

/// Whether the skin's parts are within what the client reads (26.3's profile property limits).
#[must_use]
pub fn valid_skin(skin: &Skin) -> bool {
    valid_account(&skin.from)
        && valid_base64(&skin.value, 32767)
        && skin
            .signature
            .as_deref()
            .is_none_or(|s| valid_base64(s, 1024))
}

/// Whether NPCs of this kind can be shown as babies.
#[must_use]
pub fn can_be_baby(kind: &Kind) -> bool {
    matches!(kind, Kind::Mob(name) if wire::baby_index(name).is_some())
}

/// Whether a scale is allowed: a finite number in the range of the `minecraft:scale` attribute.
#[must_use]
pub fn valid_scale(scale: f32) -> bool {
    (MIN_SCALE..=MAX_SCALE).contains(&scale)
}

/// Whether a range in blocks is allowed.
#[must_use]
pub fn valid_range(range: u32) -> bool {
    (MIN_RANGE..=MAX_RANGE).contains(&range)
}

/// The kind of an entity type name typed by the owner (`villager`, `minecraft:villager`,
/// `player`); `None` for an unknown or refused type.
#[must_use]
pub fn parse_kind(name: &str) -> Option<Kind> {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    if name == "player" || name == "mannequin" {
        return Some(Kind::Player);
    }
    if REFUSED_TYPES.contains(&name) || wire::entity_type(name).is_none() {
        return None;
    }
    Some(Kind::Mob(name.to_string()))
}

/// The name of a kind as the file and the commands write it.
#[must_use]
pub fn kind_name(kind: &Kind) -> &str {
    match kind {
        Kind::Player => "player",
        Kind::Mob(name) => name,
    }
}

#[must_use]
pub fn parse_trigger(text: &str) -> Option<Trigger> {
    match text {
        "right" => Some(Trigger::Right),
        "left" => Some(Trigger::Left),
        "any" => Some(Trigger::Any),
        _ => None,
    }
}

#[must_use]
pub fn trigger_name(trigger: Trigger) -> &'static str {
    match trigger {
        Trigger::Right => "right",
        Trigger::Left => "left",
        Trigger::Any => "any",
    }
}

/// Whether a position is one a player can stand at.
#[must_use]
pub fn valid_place(place: &Place) -> bool {
    let inside = |v: f64, limit: f64| v.is_finite() && v.abs() <= limit;
    valid_world(&place.world)
        && inside(place.x, 30_000_000.0)
        && inside(place.z, 30_000_000.0)
        && place.y.is_finite()
        && (-2048.0..=4096.0).contains(&place.y)
        && place.yaw.is_finite()
        && place.pitch.is_finite()
        && (-90.0..=90.0).contains(&place.pitch)
}

/// Why an action could not be read from a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionError {
    UnknownType,
    BadText,
    BadPlace,
    BadMenu,
    BadUi,
}

/// An action from its type and the rest of the command line. A teleport takes `x y z`, then
/// optionally `yaw pitch`, then optionally a world (else `here`'s world), or the word `here` for
/// the sender's own place.
pub fn parse_action(kind: &str, value: &str, here: Option<&Place>) -> Result<Action, ActionError> {
    let value = value.trim();
    match kind {
        "message" => valid_text(value, MAX_MESSAGE)
            .then(|| Action::message(value))
            .ok_or(ActionError::BadText),
        "command" | "console" => {
            let command = value.strip_prefix('/').unwrap_or(value).trim();
            if !valid_text(command, MAX_COMMAND) {
                return Err(ActionError::BadText);
            }
            Ok(if kind == "command" {
                Action::Command(command.to_string())
            } else {
                Action::Console(command.to_string())
            })
        }
        "menu" => valid_id(value)
            .then(|| Action::Menu(value.to_string()))
            .ok_or(ActionError::BadMenu),
        "ui" => valid_ui_file(value)
            .then(|| Action::Ui(value.to_string()))
            .ok_or(ActionError::BadUi),
        "teleport" => parse_place(value, here)
            .map(Action::Teleport)
            .ok_or(ActionError::BadPlace),
        _ => Err(ActionError::UnknownType),
    }
}

fn parse_place(value: &str, here: Option<&Place>) -> Option<Place> {
    if value == "here" {
        return here.cloned().filter(valid_place);
    }
    let words: Vec<&str> = value.split_whitespace().collect();
    let number = |i: usize| words.get(i)?.parse::<f64>().ok().filter(|v| v.is_finite());
    let (x, y, z) = (number(0)?, number(1)?, number(2)?);
    let (rotation, world_at) = match (number(3), number(4)) {
        (Some(yaw), Some(pitch)) => ((yaw as f32, pitch as f32), 5),
        _ => ((0.0, 0.0), 3),
    };
    let world = match words.get(world_at) {
        Some(world) => (*world).to_string(),
        None => here?.world.clone(),
    };
    if words.len() > world_at + 1 {
        return None;
    }
    let place = Place {
        world,
        x,
        y,
        z,
        yaw: rotation.0,
        pitch: rotation.1,
    };
    valid_place(&place).then_some(place)
}

/// An action as `info` shows it.
#[must_use]
pub fn action_text(action: &Action) -> String {
    match action {
        Action::Message { text, texts } if texts.is_empty() => format!("message {text}"),
        Action::Message { text, texts } => {
            let languages: Vec<&str> = texts.keys().map(String::as_str).collect();
            format!("message {text} ({})", languages.join(", "))
        }
        Action::Command(command) => format!("command /{command}"),
        Action::Console(command) => format!("console {command}"),
        Action::Menu(name) => format!("menu {name}"),
        Action::Ui(file) => format!("ui {file}"),
        Action::Teleport(p) => format!(
            "teleport {:.2} {:.2} {:.2} {:.1} {:.1} {}",
            p.x, p.y, p.z, p.yaw, p.pitch, p.world
        ),
    }
}

impl Store {
    /// The NPC with this number.
    #[must_use]
    pub fn by_num(&self, num: u32) -> Option<&Npc> {
        self.npcs.values().find(|npc| npc.num == num)
    }

    /// Makes an NPC with the defaults: its id as its name, shown, looking at the nearest player.
    pub fn create(
        &mut self,
        id: &str,
        kind: Kind,
        place: Place,
        uuid: (u64, u64),
    ) -> Result<&Npc, CreateError> {
        if !valid_id(id) || !valid_place(&place) {
            return Err(CreateError::BadId);
        }
        if self.npcs.contains_key(id) {
            return Err(CreateError::Exists);
        }
        let num = self.next_num.max(1);
        if self.npcs.len() >= MAX_NPCS || num > MAX_NUM {
            return Err(CreateError::Limit);
        }
        self.next_num = num + 1;
        let npc = Npc {
            id: id.to_string(),
            num,
            uuid,
            place,
            kind,
            name: Some(id.to_string()),
            names: BTreeMap::new(),
            name_visible: true,
            skin: None,
            viewer_skin: false,
            skin_patch: None,
            glow: false,
            look: false,
            baby: false,
            sit: false,
            lie: false,
            scale: DEFAULT_SCALE,
            owner: None,
            persist: true,
            range: DEFAULT_RANGE,
            cooldown_ms: DEFAULT_COOLDOWN_MS,
            permission: None,
            hidden: BTreeSet::new(),
            actions: Vec::new(),
        };
        Ok(self.npcs.entry(id.to_string()).or_insert(npc))
    }

    /// Makes or replaces the NPC `key` of the plugin `owner`. The same key replaces, never a second
    /// NPC: it keeps its number and UUID, so a client that has it is simply sent it again.
    pub fn put_owned(
        &mut self,
        owner: &str,
        key: &str,
        spec: Spec,
        uuid: (u64, u64),
    ) -> Result<Put, CreateError> {
        if !valid_owner(owner) || !valid_id(key) || !valid_place(&spec.place) {
            return Err(CreateError::BadId);
        }
        let id = owned_id(owner, key);
        let earlier = self.npcs.get(&id);
        let replaced = earlier.is_some();
        let was_persisted = earlier.is_some_and(|npc| npc.persist);
        let (num, uuid, hidden) = match earlier {
            Some(npc) => (npc.num, npc.uuid, npc.hidden.clone()),
            None => {
                let num = self.next_num.max(1);
                let held = self
                    .npcs
                    .values()
                    .filter(|npc| npc.owner.as_deref() == Some(owner))
                    .count();
                if self.npcs.len() >= MAX_NPCS || held >= MAX_PER_OWNER || num > MAX_NUM {
                    return Err(CreateError::Limit);
                }
                self.next_num = num + 1;
                (num, uuid, BTreeSet::new())
            }
        };
        self.npcs.insert(
            id.clone(),
            Npc {
                id,
                num,
                uuid,
                place: spec.place,
                kind: spec.kind,
                name: spec.name,
                names: spec.names,
                name_visible: spec.name_visible,
                skin: spec.skin,
                viewer_skin: spec.viewer_skin,
                skin_patch: spec.skin_patch,
                glow: spec.glow,
                look: spec.look,
                baby: spec.baby,
                sit: spec.sit,
                lie: spec.lie,
                scale: spec.scale,
                owner: Some(owner.to_string()),
                persist: spec.persist,
                range: spec.range,
                cooldown_ms: spec.cooldown_ms,
                permission: spec.permission,
                hidden,
                actions: spec.actions,
            },
        );
        Ok(Put {
            num,
            replaced,
            was_persisted,
        })
    }

    /// The NPCs a plugin holds, by key.
    #[must_use]
    pub fn owned_keys(&self, owner: &str) -> Vec<String> {
        self.npcs
            .values()
            .filter(|npc| npc.owner.as_deref() == Some(owner))
            .map(|npc| npc.key().to_string())
            .collect()
    }

    /// Removes every NPC of the plugin `owner` that lives in memory only (`persist: false`), in one
    /// pass, and answers each one's name and number so the caller can take it from its viewers.
    /// The owner is matched exactly (`lobby` never takes `lobby-extra`'s), NPCs kept in the file stay,
    /// and so do the server owner's own (they have no owner).
    pub fn remove_memory_only_of(&mut self, owner: &str) -> Vec<(String, u32)> {
        let mut gone = Vec::new();
        self.npcs.retain(|id, npc| {
            let takes = !npc.persist && npc.owner.as_deref() == Some(owner);
            if takes {
                gone.push((id.clone(), npc.num));
            }
            !takes
        });
        gone
    }

    /// Takes over the NPCs kept in memory only from the store this one replaces (`/npc reload`).
    /// They get new numbers: the file may use theirs. A file NPC of the same name wins.
    pub fn adopt_memory_only(&mut self, old: Store) {
        for (id, mut npc) in old.npcs {
            if npc.persist || self.npcs.contains_key(&id) {
                continue;
            }
            let num = self.next_num.max(old.next_num).max(1);
            if num > MAX_NUM {
                continue;
            }
            self.next_num = num + 1;
            npc.num = num;
            self.npcs.insert(id, npc);
        }
    }

    /// The data file's text.
    #[must_use]
    pub fn to_json(&self) -> String {
        let npcs: Vec<Value> = self
            .npcs
            .values()
            .filter(|npc| npc.persist)
            .map(npc_json)
            .collect();
        let menus: Vec<Value> = self.menus.values().map(menu_json).collect();
        let file = json!({
            "version": FORMAT_VERSION,
            "next_num": self.next_num,
            "npcs": npcs,
            "menus": menus,
        });
        serde_json::to_string_pretty(&file).unwrap_or_default()
    }

    /// Reads the data file. Any broken rule refuses the whole file, with the reason.
    pub fn from_json(text: &str) -> Result<Store, String> {
        if text.len() > MAX_FILE {
            return Err(format!("the file is larger than {MAX_FILE} bytes"));
        }
        let file: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
        let file = file.as_object().ok_or("not a JSON object")?;
        if file.get("version").and_then(Value::as_u64) != Some(FORMAT_VERSION) {
            return Err(format!("not version {FORMAT_VERSION}"));
        }
        let mut store = Store::default();
        let npcs = file
            .get("npcs")
            .and_then(Value::as_array)
            .ok_or("no npcs list")?;
        if npcs.len() > MAX_NPCS {
            return Err(format!("more than {MAX_NPCS} NPCs"));
        }
        let mut nums = BTreeSet::new();
        for (i, value) in npcs.iter().enumerate() {
            let npc = read_npc(value).map_err(|e| format!("NPC {}: {e}", i + 1))?;
            if !nums.insert(npc.num) {
                return Err(format!("two NPCs have the number {}", npc.num));
            }
            if store.npcs.insert(npc.id.clone(), npc).is_some() {
                return Err(format!("NPC {}: its name is used twice", i + 1));
            }
        }
        let menus = file
            .get("menus")
            .and_then(Value::as_array)
            .ok_or("no menus list")?;
        if menus.len() > MAX_MENUS {
            return Err(format!("more than {MAX_MENUS} menus"));
        }
        for (i, value) in menus.iter().enumerate() {
            let menu = read_menu(value).map_err(|e| format!("menu {}: {e}", i + 1))?;
            if store.menus.insert(menu.name.clone(), menu).is_some() {
                return Err(format!("menu {}: its name is used twice", i + 1));
            }
        }
        let highest = nums.last().copied().unwrap_or(0);
        let next = file
            .get("next_num")
            .and_then(Value::as_u64)
            .ok_or("no next_num")?;
        let next = u32::try_from(next).map_err(|_| "next_num is too large")?;
        if next > MAX_NUM + 1 {
            return Err("next_num is too large".into());
        }
        store.next_num = next.max(highest + 1);
        Ok(store)
    }
}

fn place_json(place: &Place, object: &mut Map<String, Value>) {
    object.insert("world".into(), json!(place.world));
    object.insert("x".into(), json!(place.x));
    object.insert("y".into(), json!(place.y));
    object.insert("z".into(), json!(place.z));
    object.insert("yaw".into(), json!(place.yaw));
    object.insert("pitch".into(), json!(place.pitch));
}

fn action_json(action: &Action) -> Value {
    let mut object = Map::new();
    let (kind, value) = match action {
        Action::Message { text, .. } => ("message", Some(text)),
        Action::Command(command) => ("command", Some(command)),
        Action::Console(command) => ("console", Some(command)),
        Action::Menu(name) => ("menu", Some(name)),
        Action::Ui(file) => ("ui", Some(file)),
        Action::Teleport(place) => {
            place_json(place, &mut object);
            ("teleport", None)
        }
    };
    object.insert("type".into(), json!(kind));
    if let Some(value) = value {
        object.insert("value".into(), json!(value));
    }
    // An older version reads `value` and leaves `texts`: the message stays, in one language.
    if let Action::Message { texts, .. } = action
        && !texts.is_empty()
    {
        object.insert("texts".into(), json!(texts));
    }
    Value::Object(object)
}

fn npc_json(npc: &Npc) -> Value {
    let mut object = Map::new();
    object.insert("id".into(), json!(npc.id));
    object.insert("num".into(), json!(npc.num));
    object.insert("uuid".into(), json!(wire::uuid_string(npc.uuid)));
    place_json(&npc.place, &mut object);
    object.insert("type".into(), json!(kind_name(&npc.kind)));
    object.insert("name".into(), json!(npc.name));
    object.insert("names".into(), json!(npc.names));
    if let Some(owner) = &npc.owner {
        object.insert("owner".into(), json!(owner));
    }
    object.insert("sit".into(), json!(npc.sit));
    object.insert("lie".into(), json!(npc.lie));
    object.insert("name_visible".into(), json!(npc.name_visible));
    let skin = npc
        .skin
        .as_ref()
        .map(|skin| json!({"from": skin.from, "value": skin.value, "signature": skin.signature}));
    object.insert("skin".into(), json!(skin));
    // Written only when set, so that a file without one stays readable by an older version.
    if npc.viewer_skin {
        object.insert("viewer_skin".into(), json!(true));
    }
    if let Some(patch) = &npc.skin_patch {
        object.insert("texture".into(), json!(patch.texture));
        object.insert("model".into(), json!(model_name(patch.model)));
    }
    object.insert("glow".into(), json!(npc.glow));
    object.insert("look".into(), json!(npc.look));
    object.insert("baby".into(), json!(npc.baby));
    // Written only when set, so that a file without one stays readable by an older version.
    #[allow(clippy::float_cmp)]
    if npc.scale != DEFAULT_SCALE {
        object.insert("scale".into(), json!(npc.scale));
    }
    object.insert("range".into(), json!(npc.range));
    object.insert("cooldown_ms".into(), json!(npc.cooldown_ms));
    object.insert("permission".into(), json!(npc.permission));
    object.insert("hidden".into(), json!(npc.hidden));
    let actions: Vec<Value> = npc
        .actions
        .iter()
        .map(|(trigger, action)| {
            let mut value = action_json(action);
            if let Value::Object(object) = &mut value {
                object.insert("on".into(), json!(trigger_name(*trigger)));
            }
            value
        })
        .collect();
    object.insert("actions".into(), Value::Array(actions));
    Value::Object(object)
}

fn menu_json(menu: &Menu) -> Value {
    let buttons: Vec<Value> = menu
        .buttons
        .iter()
        .map(|button| json!({"label": button.label, "action": button.action.as_ref().map(action_json)}))
        .collect();
    json!({"name": menu.name, "title": menu.title, "buttons": buttons})
}

type Fields<'a> = &'a Map<String, Value>;

fn field<'a>(object: Fields<'a>, key: &str) -> Result<&'a Value, String> {
    object.get(key).ok_or_else(|| format!("no {key}"))
}

fn text_field(object: Fields<'_>, key: &str) -> Result<String, String> {
    field(object, key)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{key} is not text"))
}

fn optional_text(object: Fields<'_>, key: &str) -> Result<Option<String>, String> {
    match field(object, key)? {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text.clone())),
        _ => Err(format!("{key} is not text")),
    }
}

fn number(object: Fields<'_>, key: &str) -> Result<f64, String> {
    field(object, key)?
        .as_f64()
        .ok_or_else(|| format!("{key} is not a number"))
}

fn flag(object: Fields<'_>, key: &str) -> Result<bool, String> {
    field(object, key)?
        .as_bool()
        .ok_or_else(|| format!("{key} is not true or false"))
}

fn read_place(object: Fields<'_>) -> Result<Place, String> {
    let place = Place {
        world: text_field(object, "world")?,
        x: number(object, "x")?,
        y: number(object, "y")?,
        z: number(object, "z")?,
        yaw: number(object, "yaw")? as f32,
        pitch: number(object, "pitch")? as f32,
    };
    valid_place(&place)
        .then_some(place)
        .ok_or_else(|| "the place is outside the world".into())
}

fn read_action(value: &Value) -> Result<Action, String> {
    let object = value.as_object().ok_or("an action is not an object")?;
    let kind = text_field(object, "type")?;
    if kind == "teleport" {
        return read_place(object).map(Action::Teleport);
    }
    let text = text_field(object, "value")?;
    let mut action =
        parse_action(&kind, &text, None).map_err(|e| format!("bad {kind} action: {e:?}"))?;
    // The file holds the command without its `/`; one with it was not written by the plugin.
    if action_json(&action).get("value") != Some(&Value::String(text)) {
        return Err(format!("bad {kind} action"));
    }
    if let (Action::Message { texts, .. }, Some(list)) = (&mut action, object.get("texts")) {
        let list = list.as_object().ok_or("texts is not an object")?;
        for (language, line) in list {
            let line = line.as_str().ok_or("texts holds a non-text")?;
            texts.insert(language.clone(), line.to_string());
        }
        if !valid_texts(texts, MAX_MESSAGE) {
            return Err("bad message texts".into());
        }
    }
    Ok(action)
}

fn read_npc(value: &Value) -> Result<Npc, String> {
    let object = value.as_object().ok_or("not an object")?;
    let id = text_field(object, "id")?;
    let owner = match object.get("owner") {
        None => None,
        Some(value) => Some(value.as_str().ok_or("owner is not text")?.to_string()),
    };
    let right_id = match &owner {
        None => valid_id(&id),
        Some(owner) => {
            valid_owner(owner)
                && id
                    .strip_prefix(owner.as_str())
                    .and_then(|rest| rest.strip_prefix('.'))
                    .is_some_and(valid_id)
        }
    };
    if !right_id {
        return Err("bad id".into());
    }
    let num = field(object, "num")?
        .as_u64()
        .ok_or("num is not a number")?;
    let num = u32::try_from(num)
        .ok()
        .filter(|n| (1..=MAX_NUM).contains(n))
        .ok_or("bad num")?;
    let uuid = wire::parse_uuid(&text_field(object, "uuid")?).ok_or("bad uuid")?;
    let place = read_place(object)?;
    let kind = parse_kind(&text_field(object, "type")?).ok_or("unknown type")?;
    let name = optional_text(object, "name")?;
    if name.as_deref().is_some_and(|n| !valid_text(n, MAX_NAME)) {
        return Err("bad name".into());
    }
    let skin = match field(object, "skin")? {
        Value::Null => None,
        Value::Object(skin) => {
            let skin = Skin {
                from: text_field(skin, "from")?,
                value: text_field(skin, "value")?,
                signature: optional_text(skin, "signature")?,
            };
            if !valid_skin(&skin) {
                return Err("bad skin".into());
            }
            Some(skin)
        }
        _ => return Err("skin is not an object".into()),
    };
    // Written only when set, so a file without it stays readable by an older version.
    let viewer_skin = match object.get("viewer_skin") {
        None => false,
        Some(value) => value.as_bool().ok_or("viewer_skin is not true or false")?,
    };
    // Written by later versions only, and only when set.
    let skin_patch = match object.get("texture") {
        None => None,
        Some(value) => {
            let texture = value
                .as_str()
                .filter(|t| valid_texture(t))
                .ok_or("bad texture")?;
            let model = match object.get("model") {
                None => SkinModel::Wide,
                Some(value) => value.as_str().and_then(parse_model).ok_or("bad model")?,
            };
            Some(SkinPatch {
                texture: texture.to_string(),
                model,
            })
        }
    };
    let cooldown = field(object, "cooldown_ms")?
        .as_u64()
        .ok_or("cooldown_ms is not a number")?;
    let cooldown_ms = u32::try_from(cooldown)
        .ok()
        .filter(|c| *c <= MAX_COOLDOWN_MS)
        .ok_or("bad cooldown_ms")?;
    let permission = optional_text(object, "permission")?;
    if permission.as_deref().is_some_and(|p| !valid_node(p)) {
        return Err("bad permission".into());
    }
    let hidden_list = field(object, "hidden")?
        .as_array()
        .ok_or("hidden is not a list")?;
    if hidden_list.len() > MAX_HIDDEN {
        return Err("hidden is too long".into());
    }
    let mut hidden = BTreeSet::new();
    for entry in hidden_list {
        let text = entry.as_str().ok_or("hidden holds a non-text")?;
        let id = wire::parse_uuid(text).ok_or("hidden holds a bad uuid")?;
        hidden.insert(wire::uuid_string(id));
    }
    let action_list = field(object, "actions")?
        .as_array()
        .ok_or("actions is not a list")?;
    if action_list.len() > MAX_ACTIONS {
        return Err(format!("more than {MAX_ACTIONS} actions"));
    }
    let mut actions = Vec::new();
    for entry in action_list {
        let on = entry
            .get("on")
            .and_then(Value::as_str)
            .and_then(parse_trigger)
            .ok_or("bad action trigger")?;
        actions.push((on, read_action(entry)?));
    }
    // Written by later versions only; an older file reads as the defaults.
    let names = match object.get("names") {
        None => BTreeMap::new(),
        Some(value) => {
            let list = value.as_object().ok_or("names is not an object")?;
            let mut names = BTreeMap::new();
            for (language, text) in list {
                let text = text.as_str().ok_or("names holds a non-text")?;
                names.insert(language.clone(), text.to_string());
            }
            if !valid_names(&names) {
                return Err("bad names".into());
            }
            names
        }
    };
    let sit = match object.get("sit") {
        None => false,
        Some(value) => value.as_bool().ok_or("sit is not true or false")?,
    };
    let lie = match object.get("lie") {
        None => false,
        Some(value) => value.as_bool().ok_or("lie is not true or false")?,
    };
    if sit && lie {
        return Err(SIT_AND_LIE.into());
    }
    let baby = match object.get("baby") {
        None => false,
        Some(value) => value.as_bool().ok_or("baby is not true or false")? && can_be_baby(&kind),
    };
    let range = match object.get("range") {
        None => DEFAULT_RANGE,
        Some(value) => value
            .as_u64()
            .and_then(|r| u32::try_from(r).ok())
            .filter(|r| valid_range(*r))
            .ok_or("bad range")?,
    };
    let scale = match object.get("scale") {
        None => DEFAULT_SCALE,
        #[allow(clippy::cast_possible_truncation)]
        Some(value) => value
            .as_f64()
            .map(|scale| scale as f32)
            .filter(|scale| valid_scale(*scale))
            .ok_or("bad scale")?,
    };
    Ok(Npc {
        id,
        num,
        uuid,
        place,
        kind,
        name,
        names,
        name_visible: flag(object, "name_visible")?,
        skin,
        viewer_skin,
        skin_patch,
        glow: flag(object, "glow")?,
        look: flag(object, "look")?,
        baby,
        sit,
        lie,
        scale,
        owner,
        persist: true,
        range,
        cooldown_ms,
        permission,
        hidden,
        actions,
    })
}

fn read_menu(value: &Value) -> Result<Menu, String> {
    let object = value.as_object().ok_or("not an object")?;
    let name = text_field(object, "name")?;
    if !valid_id(&name) {
        return Err("bad name".into());
    }
    let title = text_field(object, "title")?;
    if !valid_text(&title, MAX_NAME) {
        return Err("bad title".into());
    }
    let list = field(object, "buttons")?
        .as_array()
        .ok_or("buttons is not a list")?;
    if list.len() > MAX_BUTTONS {
        return Err(format!("more than {MAX_BUTTONS} buttons"));
    }
    let mut buttons = Vec::new();
    for entry in list {
        let button = entry.as_object().ok_or("a button is not an object")?;
        let label = text_field(button, "label")?;
        if !valid_text(&label, MAX_NAME) {
            return Err("bad button label".into());
        }
        let action = match field(button, "action")? {
            Value::Null => None,
            action => Some(read_action(action)?),
        };
        buttons.push(Button { label, action });
    }
    Ok(Menu {
        name,
        title,
        buttons,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn here() -> Place {
        Place {
            world: "world".into(),
            x: 0.5,
            y: 64.0,
            z: -3.25,
            yaw: 90.0,
            pitch: 10.0,
        }
    }

    fn full_store() -> Store {
        let mut store = Store::default();
        store.create("guide", Kind::Player, here(), (1, 2)).unwrap();
        store
            .create(
                "trader",
                parse_kind("minecraft:villager").unwrap(),
                here(),
                (3, 4),
            )
            .unwrap();
        let guide = store.npcs.get_mut("guide").unwrap();
        guide.name = Some("&aGuide ç".into());
        guide.skin = Some(Skin {
            from: "Notch".into(),
            value: "dGV4dA==".into(),
            signature: Some("c2ln".into()),
        });
        guide.glow = true;
        guide.names.insert("tr".into(), "&aRehber".into());
        guide.sit = true;
        guide.permission = Some("server.vip".into());
        guide.hidden.insert(wire::uuid_string((5, 6)));
        guide
            .actions
            .push((Trigger::Any, Action::message("Hello {player}")));
        guide.actions.push((
            Trigger::Right,
            Action::Message {
                text: "Welcome".into(),
                texts: BTreeMap::from([
                    ("fr".to_string(), "Bienvenue".to_string()),
                    ("tr".to_string(), "Hoş geldin".to_string()),
                ]),
            },
        ));
        guide.skin_patch = Some(SkinPatch {
            texture: "br:npc/watcher".into(),
            model: SkinModel::Slim,
        });
        guide
            .actions
            .push((Trigger::Right, Action::Command("spawn".into())));
        guide
            .actions
            .push((Trigger::Left, Action::Console("give {player} bread".into())));
        guide
            .actions
            .push((Trigger::Right, Action::Ui("notebook".into())));
        guide.actions.push((Trigger::Any, Action::Teleport(here())));
        guide
            .actions
            .push((Trigger::Any, Action::Menu("shop".into())));
        let trader = store.npcs.get_mut("trader").unwrap();
        trader.name = None;
        trader.baby = true;
        trader.lie = true;
        trader.scale = 3.0;
        trader.range = 64;
        store.menus.insert(
            "shop".into(),
            Menu {
                name: "shop".into(),
                title: "Shop".into(),
                buttons: vec![
                    Button {
                        label: "Spawn".into(),
                        action: Some(Action::Teleport(here())),
                    },
                    Button {
                        label: "Nothing".into(),
                        action: None,
                    },
                ],
            },
        );
        store
    }

    #[test]
    fn the_file_keeps_every_field() {
        let store = full_store();
        let text = store.to_json();
        assert_eq!(Store::from_json(&text), Ok(store));
    }

    #[test]
    fn a_pack_texture_is_checked_and_only_written_when_set() {
        for good in [
            "br:npc/watcher",
            "entity/player/wide/steve",
            "a.b-c_d:x/y.z",
            "minecraft:entity/player/slim/alex",
        ] {
            assert!(valid_texture(good), "{good}");
        }
        for bad in [
            "",
            "Br:x",
            "a:b:c",
            "a:",
            ":b",
            "a b",
            "a:x y",
            "é:x",
            &"a".repeat(257),
        ] {
            assert!(!valid_texture(bad), "{bad}");
        }
        assert_eq!(parse_model("wide"), Some(SkinModel::Wide));
        assert_eq!(parse_model("slim"), Some(SkinModel::Slim));
        assert_eq!(parse_model("Slim"), None);
        assert_eq!(parse_model("classic"), None);
        let text = full_store().to_json();
        assert!(text.contains("\"texture\": \"br:npc/watcher\""), "{text}");
        assert_eq!(
            text.matches("\"texture\"").count(),
            1,
            "an NPC without one writes nothing new, so an older version reads the file"
        );
        assert!(Store::from_json(&text.replace("br:npc/watcher", "BR:npc")).is_err());
        assert!(Store::from_json(&text.replace("\"slim\"", "\"classic\"")).is_err());
    }

    #[test]
    fn a_message_has_a_line_per_language_and_the_file_checks_them() {
        let text = full_store().to_json();
        // An older version reads `value` and leaves `texts`: the message stays, in one language.
        assert!(text.contains("\"value\": \"Welcome\""), "{text}");
        assert!(text.contains("\"texts\""), "{text}");
        assert_eq!(
            text.matches("\"texts\"").count(),
            1,
            "only where there are some"
        );
        assert!(
            Store::from_json(&text.replace("\"fr\": \"Bienvenue\"", "\"de\": \"Willkommen\""))
                .is_err()
        );
        assert!(Store::from_json(&text.replace("\"Bienvenue\"", "\"\"")).is_err());
        let welcome = Action::Message {
            text: "Welcome".into(),
            texts: BTreeMap::from([("tr".to_string(), "Hoş geldin".to_string())]),
        };
        assert_eq!(action_text(&welcome), "message Welcome (tr)");
        assert_eq!(action_text(&Action::message("Hi")), "message Hi");
        let Action::Message { text, texts } = &welcome else {
            unreachable!()
        };
        assert_eq!(line_for(text, texts, "tr"), "Hoş geldin");
        assert_eq!(
            line_for(text, texts, "fr"),
            "Welcome",
            "no French line: the text"
        );
        assert_eq!(line_for(text, texts, "en"), "Welcome");
    }

    #[test]
    fn a_plugin_npc_keeps_its_actions_and_pack_texture() {
        let mut store = Store::default();
        let mut given = spec();
        given.actions = vec![(Trigger::Right, Action::message("Hi"))];
        given.skin_patch = Some(SkinPatch {
            texture: "br:npc/watcher".into(),
            model: SkinModel::Wide,
        });
        store
            .put_owned("core", "boss", given.clone(), (1, 1))
            .unwrap();
        let boss = &store.npcs["core.boss"];
        assert_eq!(boss.actions, given.actions);
        assert_eq!(boss.skin_patch, given.skin_patch);
    }

    #[test]
    fn a_viewer_skin_is_kept_and_only_written_when_set() {
        let mut store = Store::default();
        let mut given = spec();
        given.viewer_skin = true;
        store.put_owned("lobby", "guide", given, (1, 1)).unwrap();
        store.put_owned("lobby", "boss", spec(), (2, 2)).unwrap();
        assert!(store.npcs["lobby.guide"].viewer_skin);
        let text = store.to_json();
        assert_eq!(
            text.matches("viewer_skin").count(),
            1,
            "only the guide writes it"
        );
        let read = Store::from_json(&text).unwrap();
        assert_eq!(read, store);
        assert!(!read.npcs["lobby.boss"].viewer_skin);
        let broken = text.replace(r#""viewer_skin": true"#, r#""viewer_skin": "yes""#);
        assert_ne!(broken, text);
        assert!(Store::from_json(&broken).is_err());
    }

    #[test]
    fn range_and_baby_are_checked_and_older_files_read_their_defaults() {
        assert!(valid_range(MIN_RANGE) && valid_range(DEFAULT_RANGE) && valid_range(MAX_RANGE));
        assert!(!valid_range(MIN_RANGE - 1) && !valid_range(MAX_RANGE + 1));
        assert!(can_be_baby(&Kind::Mob("villager".into())));
        assert!(!can_be_baby(&Kind::Mob("allay".into())));
        assert!(!can_be_baby(&Kind::Player));
        let mut store = Store::default();
        let npc = store.create("a", Kind::Player, here(), (1, 1)).unwrap();
        assert!(!npc.look, "an NPC does not turn unless told to");
        assert_eq!(npc.range, DEFAULT_RANGE);
        let text = full_store().to_json();
        let old = text
            .replace("\"baby\": true,", "")
            .replace("\"range\": 64,", "");
        let old = old
            .replace("\"baby\": false,", "")
            .replace("\"range\": 48,", "");
        assert!(!old.contains("baby") && !old.contains("range"));
        let read = Store::from_json(&old).unwrap();
        assert!(!read.npcs["trader"].baby);
        assert_eq!(read.npcs["trader"].range, DEFAULT_RANGE);
        assert!(Store::from_json(&text.replace("\"range\": 64", "\"range\": 500")).is_err());
        assert!(Store::from_json(&text.replace("\"range\": 64", "\"range\": -1")).is_err());
        // A baby flag on a type without one reads as false.
        let wrong = text.replace("\"baby\": false", "\"baby\": true");
        assert!(!Store::from_json(&wrong).unwrap().npcs["guide"].baby);
    }

    #[test]
    fn a_scale_is_checked_kept_in_the_file_and_an_older_file_reads_one() {
        assert!(valid_scale(MIN_SCALE) && valid_scale(DEFAULT_SCALE) && valid_scale(MAX_SCALE));
        assert!(!valid_scale(0.0) && !valid_scale(MAX_SCALE + 0.5) && !valid_scale(-1.0));
        assert!(!valid_scale(f32::NAN) && !valid_scale(f32::INFINITY));
        let text = full_store().to_json();
        let read = Store::from_json(&text).unwrap();
        assert!((read.npcs["trader"].scale - 3.0).abs() < f32::EPSILON);
        assert!((read.npcs["guide"].scale - DEFAULT_SCALE).abs() < f32::EPSILON);
        // The default is not written, so an older version reads the file.
        assert_eq!(text.matches("\"scale\"").count(), 1);
        assert!(Store::from_json(&text.replace("\"scale\": 3.0", "\"scale\": 40")).is_err());
        assert!(Store::from_json(&text.replace("\"scale\": 3.0", "\"scale\": \"big\"")).is_err());
        let old = Store::from_json(&text.replace("\"scale\": 3.0,", "")).unwrap();
        assert!((old.npcs["trader"].scale - DEFAULT_SCALE).abs() < f32::EPSILON);
    }

    fn spec() -> Spec {
        Spec {
            kind: Kind::Player,
            place: here(),
            name: Some("&2Boss".into()),
            names: BTreeMap::new(),
            name_visible: true,
            skin: None,
            viewer_skin: false,
            skin_patch: None,
            glow: false,
            look: false,
            baby: false,
            sit: false,
            lie: false,
            scale: DEFAULT_SCALE,
            persist: true,
            range: 112,
            cooldown_ms: 1000,
            permission: None,
            actions: Vec::new(),
        }
    }

    #[test]
    fn a_create_on_an_existing_key_answers_the_same_entity_id() {
        let mut store = Store::default();
        let first = store.put_owned("core", "boss", spec(), (1, 1)).unwrap();
        let again = store.put_owned("core", "boss", spec(), (2, 2)).unwrap();
        assert!(again.replaced && !first.replaced);
        assert_eq!(entity_id(again.num), entity_id(first.num));
        assert_eq!(store.npcs["core.boss"].entity_id(), entity_id(first.num));
        assert_eq!(num_of(store.npcs["core.boss"].entity_id()), Some(first.num));
    }

    #[test]
    fn the_same_key_replaces_and_keys_are_scoped_to_the_plugin() {
        let mut store = Store::default();
        let first = store.put_owned("core", "boss", spec(), (1, 1)).unwrap();
        assert_eq!((first.num, first.replaced), (1, false));
        store
            .npcs
            .get_mut("core.boss")
            .unwrap()
            .hidden
            .insert(wire::uuid_string((5, 5)));
        let mut changed = spec();
        changed.place.x = 9.0;
        let second = store.put_owned("core", "boss", changed, (2, 2)).unwrap();
        assert_eq!((second.num, second.replaced), (1, true));
        assert!(second.was_persisted);
        assert_eq!(store.npcs.len(), 1, "never a second NPC");
        let boss = &store.npcs["core.boss"];
        assert_eq!(boss.uuid, (1, 1), "the same entity on a client");
        assert_eq!(boss.place.x, 9.0);
        assert_eq!(boss.owner.as_deref(), Some("core"));
        assert_eq!(boss.hidden.len(), 1, "who it is hidden from stays");
        let other = store.put_owned("other", "boss", spec(), (3, 3)).unwrap();
        assert_eq!((other.num, other.replaced), (2, false));
        assert_eq!(store.owned_keys("core"), vec!["boss".to_string()]);
        assert_eq!(store.owned_keys("cor"), Vec::<String>::new());
        assert_eq!(
            store.put_owned("core", "Bad Key", spec(), (4, 4)),
            Err(CreateError::BadId)
        );
        assert_eq!(
            store.put_owned("a/b", "boss", spec(), (4, 4)),
            Err(CreateError::BadId)
        );
    }

    #[test]
    fn one_plugin_cannot_use_up_the_server_limit() {
        let mut store = Store::default();
        for i in 0..MAX_PER_OWNER {
            store
                .put_owned("core", &format!("n{i}"), spec(), (0, i as u64))
                .unwrap();
        }
        assert_eq!(
            store.put_owned("core", "last", spec(), (9, 9)),
            Err(CreateError::Limit)
        );
        // Replacing is still fine, and so is another owner.
        assert!(store.put_owned("core", "n1", spec(), (9, 9)).is_ok());
        assert!(store.put_owned("other", "n1", spec(), (9, 9)).is_ok());
        assert!(store.create("mine", Kind::Player, here(), (8, 8)).is_ok());
    }

    #[test]
    fn memory_only_npcs_stay_out_of_the_file_and_survive_a_reload() {
        let mut store = Store::default();
        let mut temporary = spec();
        temporary.persist = false;
        store.put_owned("core", "kept", spec(), (1, 1)).unwrap();
        store.put_owned("core", "temp", temporary, (2, 2)).unwrap();
        let text = store.to_json();
        assert!(text.contains("core.kept") && !text.contains("core.temp"));
        let mut read = Store::from_json(&text).unwrap();
        let kept = &read.npcs["core.kept"];
        assert_eq!(kept.owner.as_deref(), Some("core"));
        assert!(kept.persist);
        assert_eq!(read.npcs.len(), 1);
        // A file that took the temporary NPC's number: it gets a new one.
        read.npcs.get_mut("core.kept").unwrap().num = store.npcs["core.temp"].num;
        read.next_num = 3;
        let old_num = store.npcs["core.temp"].num;
        read.adopt_memory_only(store);
        assert_eq!(read.npcs.len(), 2);
        let temp = &read.npcs["core.temp"];
        assert!(!temp.persist);
        assert_ne!(temp.num, old_num);
        assert_eq!(read.next_num, temp.num + 1);
    }

    fn put(store: &mut Store, owner: &str, key: &str, persist: bool, uuid: u64) -> u32 {
        let mut spec = spec();
        spec.persist = persist;
        store.put_owned(owner, key, spec, (uuid, uuid)).unwrap().num
    }

    #[test]
    fn a_disable_removes_the_non_persistent_figures_of_that_script_and_keeps_its_persistent_ones() {
        let mut store = Store::default();
        let temp = put(&mut store, "lobby", "guide", false, 1);
        let other_temp = put(&mut store, "lobby", "porter", false, 2);
        put(&mut store, "lobby", "keeper", true, 3);
        let mut gone = store.remove_memory_only_of("lobby");
        gone.sort();
        assert_eq!(
            gone,
            vec![
                ("lobby.guide".to_string(), temp),
                ("lobby.porter".to_string(), other_temp)
            ]
        );
        assert_eq!(store.npcs.keys().collect::<Vec<_>>(), vec!["lobby.keeper"]);
        assert!(
            store.remove_memory_only_of("lobby").is_empty(),
            "nothing is left to take"
        );
    }

    #[test]
    fn a_disable_leaves_other_scripts_and_the_server_owners_figures() {
        let mut store = Store::default();
        put(&mut store, "lobby", "guide", false, 1);
        put(&mut store, "other", "guide", false, 2);
        store.create("mine", Kind::Player, here(), (8, 8)).unwrap();
        store.npcs.get_mut("mine").unwrap().persist = false;
        let gone = store.remove_memory_only_of("lobby");
        assert_eq!(gone.len(), 1);
        assert!(store.npcs.contains_key("other.guide"));
        assert!(
            store.npcs.contains_key("mine"),
            "a /npc figure has no owner script"
        );
        assert!(store.remove_memory_only_of("nobody").is_empty());
        assert_eq!(store.npcs.len(), 2);
    }

    #[test]
    fn a_disable_names_the_owner_exactly_and_never_by_prefix() {
        let mut store = Store::default();
        put(&mut store, "lobby", "guide", false, 1);
        put(&mut store, "lobby-extra", "guide", false, 2);
        put(&mut store, "lob", "guide", false, 3);
        let gone = store.remove_memory_only_of("lobby");
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].0, "lobby.guide");
        let mut left: Vec<_> = store.npcs.keys().cloned().collect();
        left.sort();
        assert_eq!(left, vec!["lob.guide", "lobby-extra.guide"]);
    }

    #[test]
    fn a_name_is_read_per_language_and_the_file_checks_owner_and_names() {
        let mut store = full_store();
        let guide = store.npcs.get_mut("guide").unwrap();
        assert_eq!(guide.name_for("tr"), Some("&aRehber"));
        assert_eq!(guide.name_for("fr"), Some("&aGuide ç"));
        guide.name = None;
        assert_eq!(guide.name_for("fr"), None);
        let good = full_store().to_json();
        assert!(Store::from_json(&good.replace("\"tr\": \"&aRehber\"", "\"de\": \"x\"")).is_err());
        assert!(Store::from_json(&good.replace("\"tr\": \"&aRehber\"", "\"tr\": \"\"")).is_err());
        let mut owned = Store::default();
        owned.put_owned("core", "boss", spec(), (1, 1)).unwrap();
        let text = owned.to_json();
        assert!(Store::from_json(&text).is_ok());
        // An id that does not match its owner is refused, so a file cannot move an NPC to a plugin.
        assert!(Store::from_json(&text.replace("core.boss", "other.boss")).is_err());
        assert!(
            Store::from_json(&text.replace("\"owner\": \"core\"", "\"owner\": \"a/b\"")).is_err()
        );
        let older = text
            .replace("\"names\": {},", "")
            .replace("\"sit\": false,", "")
            .replace("\"lie\": false,", "");
        assert!(
            !older.contains("names") && !older.contains("\"sit\"") && !older.contains("\"lie\"")
        );
        assert!(Store::from_json(&older).is_ok());
    }

    #[test]
    fn a_lying_npc_is_kept_and_never_also_sits() {
        let store = full_store();
        assert!(store.npcs["trader"].lie && !store.npcs["trader"].sit);
        assert!(!store.npcs["guide"].lie && store.npcs["guide"].sit);
        let text = store.to_json();
        assert!(text.contains("\"lie\": true"));
        assert!(Store::from_json(&text).unwrap().npcs["trader"].lie);
        // A file that has an NPC both sitting and lying is refused, as a plugin's spec is.
        let both = text.replace("\"lie\": false", "\"lie\": true");
        assert!(Store::from_json(&both).unwrap_err().contains(SIT_AND_LIE));
        assert!(Store::from_json(&text.replace("\"lie\": true", "\"lie\": 1")).is_err());
        let mut owned = Store::default();
        let mut lying = spec();
        lying.lie = true;
        owned.put_owned("core", "boss", lying, (1, 1)).unwrap();
        assert!(owned.npcs["core.boss"].lie);
    }

    #[test]
    fn a_seat_has_its_own_id_and_uuid_and_rests_the_feet_at_the_place() {
        let store = full_store();
        let guide = &store.npcs["guide"];
        assert_eq!(guide.seat_id(), SEAT_ID_BASE + 1);
        assert_eq!(num_of(guide.seat_id()), None, "a seat is not clickable");
        assert_ne!(guide.seat_uuid(), guide.uuid);
        assert!(wire::entity_type(SEAT_TYPE).is_some());
        assert!((guide.seat_y() - 64.6).abs() < 1e-9);
        assert!((store.npcs["trader"].seat_y() - 64.0).abs() < 1e-9);
    }

    #[test]
    fn numbers_are_given_once_and_never_reused() {
        let mut store = Store::default();
        assert_eq!(
            store.create("a", Kind::Player, here(), (1, 1)).unwrap().num,
            1
        );
        assert_eq!(
            store.create("b", Kind::Player, here(), (1, 2)).unwrap().num,
            2
        );
        store.npcs.remove("b");
        assert_eq!(
            store.create("c", Kind::Player, here(), (1, 3)).unwrap().num,
            3
        );
        assert_eq!(
            store.create("a", Kind::Player, here(), (1, 4)).unwrap_err(),
            CreateError::Exists
        );
        assert_eq!(
            store.create("A", Kind::Player, here(), (1, 4)).unwrap_err(),
            CreateError::BadId
        );
        let text = store.to_json();
        let mut again = Store::from_json(&text).unwrap();
        assert_eq!(
            again.create("d", Kind::Player, here(), (1, 5)).unwrap().num,
            4
        );
        assert_eq!(again.by_num(3).map(|n| n.id.as_str()), Some("c"));
        assert_eq!(num_of(entity_id(3)), Some(3));
        assert_eq!(num_of(entity_id(0)), None);
        assert_eq!(num_of(5), None);
        assert_eq!(num_of(-1), None);
    }

    #[test]
    fn the_npc_limit_holds() {
        let mut store = Store::default();
        for i in 0..MAX_NPCS {
            store
                .create(&format!("n{i}"), Kind::Player, here(), (0, i as u64))
                .unwrap();
        }
        assert_eq!(
            store
                .create("last", Kind::Player, here(), (9, 9))
                .unwrap_err(),
            CreateError::Limit
        );
    }

    #[test]
    fn a_broken_file_is_refused_whole() {
        let good = full_store().to_json();
        assert!(Store::from_json("").is_err());
        assert!(Store::from_json("not json").is_err());
        assert!(Store::from_json("[]").is_err());
        assert!(Store::from_json(&good.replace("\"version\": 1", "\"version\": 2")).is_err());
        assert!(
            Store::from_json(&good.replace("\"id\": \"guide\"", "\"id\": \"Guide!\"")).is_err()
        );
        assert!(
            Store::from_json(&good.replace("\"id\": \"trader\"", "\"id\": \"guide\"")).is_err()
        );
        assert!(Store::from_json(&good.replace("\"num\": 2", "\"num\": 1")).is_err());
        assert!(Store::from_json(&good.replacen("\"y\": 64.0", "\"y\": 99999.0", 1)).is_err());
        assert!(Store::from_json(&good.replacen("\"pitch\": 10.0", "\"pitch\": 95.0", 1)).is_err());
        assert!(
            Store::from_json(&good.replace("\"type\": \"villager\"", "\"type\": \"player_x\""))
                .is_err()
        );
        assert!(
            Store::from_json(&good.replace("\"type\": \"villager\"", "\"type\": \"marker\""))
                .is_err()
        );
        assert!(
            Store::from_json(&good.replace("\"value\": \"spawn\"", "\"value\": \"/spawn\""))
                .is_err()
        );
        assert!(Store::from_json(&good.replace("server.vip", "Server VIP")).is_err());
        assert!(Store::from_json(&good.replace("dGV4dA==", "not base64!")).is_err());
        assert!(
            Store::from_json(&good.replace("\"on\": \"right\"", "\"on\": \"middle\"")).is_err()
        );
        let long = "x".repeat(MAX_NAME + 1);
        assert!(Store::from_json(&good.replace("&aGuide ç", &long)).is_err());
        let many: Vec<String> = (0..=MAX_ACTIONS)
            .map(|_| "{\"on\":\"any\",\"type\":\"message\",\"value\":\"hi\"}".to_string())
            .collect();
        let trader_actions = format!("\"actions\": [{}]", many.join(","));
        assert!(Store::from_json(&good.replace("\"actions\": []", &trader_actions)).is_err());
        assert!(Store::from_json(&"x".repeat(MAX_FILE + 1)).is_err());
    }

    #[test]
    fn actions_read_from_a_command_line() {
        let place = here();
        assert_eq!(
            parse_action("message", " Hi {player} ", None),
            Ok(Action::message("Hi {player}"))
        );
        assert_eq!(
            parse_action("command", "/warp spawn", None),
            Ok(Action::Command("warp spawn".into()))
        );
        assert_eq!(
            parse_action("console", "say hi", None),
            Ok(Action::Console("say hi".into()))
        );
        assert_eq!(
            parse_action("menu", "shop", None),
            Ok(Action::Menu("shop".into()))
        );
        assert_eq!(
            parse_action("menu", "Shop!", None),
            Err(ActionError::BadMenu)
        );
        assert_eq!(
            parse_action("ui", "notebook", None),
            Ok(Action::Ui("notebook".into()))
        );
        assert_eq!(
            parse_action("ui", "backrooms:map.v2", None),
            Ok(Action::Ui("backrooms:map.v2".into()))
        );
        let long = "a".repeat(65);
        for bad in [
            "",
            "Notebook",
            "../notebook",
            "screens/notebook",
            long.as_str(),
        ] {
            assert_eq!(
                parse_action("ui", bad, None),
                Err(ActionError::BadUi),
                "{bad}"
            );
        }
        assert_eq!(
            parse_action("teleport", "here", Some(&place)),
            Ok(Action::Teleport(place.clone()))
        );
        let at = |x, y, z, yaw, pitch, world: &str| {
            Ok(Action::Teleport(Place {
                world: world.into(),
                x,
                y,
                z,
                yaw,
                pitch,
            }))
        };
        assert_eq!(
            parse_action("teleport", "1 2 3", Some(&place)),
            at(1.0, 2.0, 3.0, 0.0, 0.0, "world")
        );
        assert_eq!(
            parse_action("teleport", "1 2 3 90 -10", Some(&place)),
            at(1.0, 2.0, 3.0, 90.0, -10.0, "world")
        );
        assert_eq!(
            parse_action("teleport", "1 2 3 world_nether", None),
            at(1.0, 2.0, 3.0, 0.0, 0.0, "world_nether")
        );
        assert_eq!(
            parse_action("teleport", "1 2 3 90 0 world_nether", None),
            at(1.0, 2.0, 3.0, 90.0, 0.0, "world_nether")
        );
        assert_eq!(
            parse_action("teleport", "1 2 3", None),
            Err(ActionError::BadPlace)
        );
        assert_eq!(
            parse_action("teleport", "1 2", Some(&place)),
            Err(ActionError::BadPlace)
        );
        assert_eq!(
            parse_action("teleport", "1 2 3 a b", Some(&place)),
            Err(ActionError::BadPlace)
        );
        assert_eq!(
            parse_action("teleport", "1 NaN 3", Some(&place)),
            Err(ActionError::BadPlace)
        );
        assert_eq!(
            parse_action("teleport", "1 2 3 0 100", Some(&place)),
            Err(ActionError::BadPlace)
        );
        assert_eq!(parse_action("message", "", None), Err(ActionError::BadText));
        assert_eq!(
            parse_action("message", "a\u{7}b", None),
            Err(ActionError::BadText)
        );
        assert_eq!(
            parse_action("command", "/", None),
            Err(ActionError::BadText)
        );
        assert_eq!(
            parse_action("dance", "x", None),
            Err(ActionError::UnknownType)
        );
    }

    #[test]
    fn kinds_and_names_are_checked() {
        assert_eq!(parse_kind("player"), Some(Kind::Player));
        assert_eq!(parse_kind("minecraft:mannequin"), Some(Kind::Player));
        assert_eq!(parse_kind("villager"), Some(Kind::Mob("villager".into())));
        assert_eq!(parse_kind("fishing_bobber"), None);
        assert_eq!(parse_kind("unicorn"), None);
        assert!(valid_id("shop-1_a"));
        assert!(!valid_id(""));
        assert!(!valid_id(&"a".repeat(33)));
        assert!(valid_account("Notch_2"));
        assert!(!valid_account("bad name"));
        assert!(valid_node("npc-lite:vip.guide"));
        assert!(!valid_node("vip guide"));
        let npc = full_store().npcs.remove("trader").unwrap();
        assert_eq!(npc.entity_type(), (143, 1.62));
        assert_eq!(npc.entity_id(), ENTITY_ID_BASE + 2);
    }
}
