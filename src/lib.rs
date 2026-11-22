//! npc-lite: NPCs that exist only as packets sent to the players near them.
//!
//! - **Packets only.** An NPC is never a server entity: the server keeps no entity, ticks nothing
//!   and saves nothing in the world. Each player near an NPC is sent it (`c-spawn-entity` and
//!   `c-set-entity-metadata`) and it is taken away (`c-remove-entities`) when he goes away.
//! - **One path for Java and Bedrock viewers.** A Java player is sent these packets with
//!   `java-player.send-packet`, a Bedrock player the same ones with `bedrock-player.send-java-packet`
//!   (the engine draws them in Bedrock's form); no entity is made for either.
//! - **Player NPCs** are mannequins, the player-shaped entity of 26.3 with a skin from a profile
//!   in its entity data: they never enter the tab list. The skin is a Minecraft account's signed
//!   textures, looked up through the server (`skins.lookup`, as SkinBringer does) or taken from
//!   an online player.
//! - **Mob NPCs**: any other entity type.
//! - **Range**: one scan every 10 ticks while a player is near an NPC, every 20 otherwise, no
//!   scan at all while there is no NPC; an NPC is sent inside 48 blocks and taken beyond 52.
//! - **Looking**: an NPC that looks at players turns to the nearest player within 10 blocks; the
//!   task that turns it runs only while someone is that near.
//! - **Clicks**: the client's interact and attack on an id the server does not know reach the
//!   plugin as `player-interact-unknown-entity`; the plugin checks that the player sees the NPC,
//!   is in its world and within 8 blocks, and that its cooldown passed.
//! - **Visibility**: per NPC a permission, and a list of players it is hidden from (commands and
//!   the plugin message API).
//! - **Other plugins** make and change NPCs of their own through the plugin messages (`api`).
//! - **Storage**: `data/npcs.json`, written through a temporary file off the tick; a file that
//!   cannot be read is left untouched and nothing can be changed until it is fixed.
//!
//! Written from the behaviour of NPC plugins as their public pages describe it; no other plugin's
//! code was read.

pub mod api;
pub mod dispatch;
pub mod import;
pub mod lang;
pub mod model;
pub mod types;
pub mod view;
pub mod wire;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, MutexGuard, PoisonError};

use strawberry_plugin_api::{
    Context, Player, Plugin, PluginMetadata, Result, Server,
    command::{
        Arg, ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs,
        StringType,
    },
    command_wit::Number,
    commands::CommandHandler,
    common::GameMode,
    events::{
        DialogClickActionEvent, EventData, EventHandler, ExtensionEvent, ExtensionEventHandler,
        FailureVerdict, HandlerOptions, HandlerPriority, PLUGIN_DISABLE, PlayerChangeWorldEvent,
        PlayerInteractUnknownEntityEvent, PlayerLeaveEvent, PlayerLocaleChangeEvent,
        PlayerRespawnEvent,
    },
    events_wit::EntityInteractionAction,
    java_dialog as dialog,
    java_packets::{
        CHeadRot, CRemoveEntities, CSetEntityMetadata, CSetPassengers, CSpawnEntity,
        CUpdateAttributes, CUpdateEntityRot, ClientboundPacket, Property,
    },
    logging::{LogLevel, log},
    permission::{Permission, PermissionDefault, PermissionLevel},
    permissions, register_plugin,
    scheduler::{SchedulerExt, cancel_task},
    skins::{SkinError, SkinLookupJob, lookup},
    text::TextComponent,
    uuid::Uuid,
};

use api::{Request, SkinAsk};
use model::{
    Action, Button, CreateError, Kind, Menu, Npc, Place, Skin, SkinModel, SkinPatch, Store, Trigger,
};
use view::{Candidate, ClickVerdict, Pos, Turn};

const DATA_FILE: &str = "data/npcs.json";
const ADMIN_NODE: &str = "npc-lite:admin";
/// Storing or changing a `console` action (it runs with every right): see `dispatch::may_store`.
const CONSOLE_NODE: &str = "npc-lite:admin.console";
const PLUGIN_NAME: &str = "npc-lite";
/// Scans run more often while a player is this much nearer than an NPC's range.
const NEAR_MARGIN: f64 = 16.0;
/// An NPC turns to the nearest player within this distance.
const LOOK_RANGE: f64 = 10.0;
/// The look task follows the players within this distance.
const LOOK_FOLLOW: f64 = 14.0;
/// A click counts when the player's eyes are within his entity interaction range of the NPC's box,
/// plus this leniency for lag (vanilla's check: 3 blocks, 5 in creative, plus 3).
const CLICK_LENIENCY: f64 = 3.0;
/// At most this many NPCs are sent in one scan, so one run stays short.
const SPAWN_BUDGET: usize = 512;
const LOOK_PERIOD: u64 = 2;
const SAVE_DELAY: u64 = 20;
const EYE: f64 = 1.62;
const DIALOG_PREFIX: &str = "npc-lite:menu/";
const IMPORT_FILE: &str = "data/import.yml";
/// At most this many lines of what became of each row.
const IMPORT_LINES: usize = 40;
const FILE_BROKEN: &str = "the NPC file could not be read";

type Key = (u64, u64);

/// What one player has been sent.
#[derive(Default)]
struct Viewer {
    world: String,
    visible: BTreeSet<u32>,
    /// Send `c-remove-entities` for every NPC before the next sends: a new viewer may still
    /// show NPCs of an earlier instance of the plugin, and a respawn or world change may or may
    /// not have cleared the client's entities.
    reset: bool,
    last_click: BTreeMap<u32, u64>,
    open_menu: Option<String>,
    /// His language, read when an NPC in his world has names per language; read again at a respawn
    /// or world change, and set when his client says it changed.
    language: Option<&'static str>,
    /// His language changed: the NPCs with names per language are sent to him again.
    language_changed: bool,
    /// The player's handles from the last scan, so the look task needs no lookups. Dropped when
    /// he leaves or is not seen by a scan.
    player: Option<Player>,
    /// How his packets are sent: the one path for a Java and a Bedrock viewer (`Sink`).
    sink: Option<Sink>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Refresh {
    Meta,
    Respawn,
}

struct Lookup {
    npc: String,
    account: String,
    job: SkinLookupJob,
    asker: Option<Key>,
}

#[derive(Default)]
struct State {
    store: Store,
    /// Why the data file could not be read; while set, nothing is changed or saved.
    broken: Option<String>,
    server: Option<Server>,
    dirty: bool,
    save_scheduled: bool,
    viewers: BTreeMap<Key, Viewer>,
    scan_gen: u64,
    scan_running: bool,
    refresh: BTreeMap<u32, Refresh>,
    removed: BTreeSet<u32>,
    /// Entity data per NPC and language (`""` for an NPC with one name for every language).
    meta: BTreeMap<(u32, &'static str), Vec<u8>>,
    /// The turn last sent for an NPC that turned away from its own rotation.
    rot: BTreeMap<u32, Turn>,
    look_task: Option<u32>,
    look_near: BTreeSet<Key>,
    lookups: Vec<Lookup>,
    lookup_task: Option<u32>,
    warned: BTreeSet<String>,
    /// When each player's click last ran a command (`dispatch::may_run`).
    commands_run: BTreeMap<Key, u64>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

// What the plugin's own work cost since it was loaded (`/npc stats`): runs and microseconds of the
// scan and of the look task, and the packets sent.
static SCANS: AtomicU64 = AtomicU64::new(0);
static SCAN_US: AtomicU64 = AtomicU64::new(0);
static LOOKS: AtomicU64 = AtomicU64::new(0);
static LOOK_US: AtomicU64 = AtomicU64::new(0);
static PACKETS: AtomicU64 = AtomicU64::new(0);

fn timed<T>(runs: &AtomicU64, micros: &AtomicU64, f: impl FnOnce() -> T) -> T {
    let start = std::time::Instant::now();
    let out = f();
    runs.fetch_add(1, Relaxed);
    micros.fetch_add(
        u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX),
        Relaxed,
    );
    out
}

fn lock() -> MutexGuard<'static, Option<State>> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    f(lock().get_or_insert_with(State::default))
}

fn key_of(id: &Uuid) -> Key {
    (id.high, id.low)
}

fn uuid_of(key: Key) -> Uuid {
    Uuid {
        high: key.0,
        low: key.1,
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn npc_pos(npc: &Npc) -> Pos {
    (npc.place.x, npc.place.y, npc.place.z)
}

// ---------------------------------------------------------------- storage

fn load() -> std::result::Result<Store, String> {
    match std::fs::metadata(DATA_FILE) {
        Err(_) => return Ok(Store::default()),
        Ok(meta) if meta.len() > model::MAX_FILE as u64 => {
            return Err(format!("the file is larger than {} bytes", model::MAX_FILE));
        }
        Ok(_) => {}
    }
    let text = std::fs::read_to_string(DATA_FILE).map_err(|e| e.to_string())?;
    Store::from_json(&text)
}

fn save(st: &mut State) {
    if !st.dirty || st.broken.is_some() {
        return;
    }
    let temporary = format!("{DATA_FILE}.tmp");
    let written = std::fs::write(&temporary, st.store.to_json())
        .and_then(|()| std::fs::rename(&temporary, DATA_FILE));
    match written {
        Ok(()) => st.dirty = false,
        Err(e) => log(
            LogLevel::Error,
            &format!("npc-lite: the NPCs could not be saved: {e}"),
        ),
    }
}

/// Writes the file soon, off the tick.
fn ensure_save(st: &mut State) {
    st.dirty = true;
    if st.save_scheduled {
        return;
    }
    if let Some(server) = &st.server {
        st.save_scheduled = true;
        server.schedule_delayed_async_task(SAVE_DELAY, |_| {
            with(|st| {
                st.save_scheduled = false;
                save(st);
            });
        });
    }
}

// ---------------------------------------------------------------- packets

/// Where one viewer's packets go. A Java and a Bedrock viewer are sent the same Java packets: a
/// Bedrock player's go through the engine's `bedrock-player.send-java-packet`, which draws them in
/// Bedrock's form. No entity of the world is made for either.
enum Sink {
    Java(strawberry_plugin_api::JavaPlayer),
    Bedrock(strawberry_plugin_api::BedrockPlayer),
}

impl Sink {
    fn of(player: &Player) -> Option<Self> {
        player
            .as_java()
            .map(Sink::Java)
            .or_else(|| player.as_bedrock().map(Sink::Bedrock))
    }
}

/// What a figure's packets are sent through; a test records them instead.
trait Wire {
    fn send_packet(&self, packet: &ClientboundPacket) -> std::result::Result<(), String>;
    /// Whether the engine draws these packets in Bedrock's form, which takes no attribute update.
    fn is_bedrock(&self) -> bool;
}

impl Wire for Sink {
    fn send_packet(&self, packet: &ClientboundPacket) -> std::result::Result<(), String> {
        match self {
            Sink::Java(java) => {
                java.send_packet(packet);
                Ok(())
            }
            Sink::Bedrock(bedrock) => bedrock.send_java_packet(packet),
        }
    }

    fn is_bedrock(&self) -> bool {
        matches!(self, Sink::Bedrock(_))
    }
}

static REFUSED: AtomicU64 = AtomicU64::new(0);

fn send(sink: &impl Wire, packet: &ClientboundPacket) {
    PACKETS.fetch_add(1, Relaxed);
    if let Err(why) = sink.send_packet(packet) {
        // The engine names what it has no form for; one line is enough.
        // (The host's log call does not exist in a unit test: a test that makes a refusal would abort
        // the whole test binary there.)
        if REFUSED.fetch_add(1, Relaxed) == 0 && !cfg!(test) {
            log(
                LogLevel::Warn,
                &format!(
                    "npc-lite: the engine refused a figure packet for a Bedrock player: {why}"
                ),
            );
        }
    }
}

/// The entity data of an NPC for a viewer of this language: glow, name, silence, its pose when it
/// lies, and for a player NPC its skin. `own` is the viewer's own skin, drawn on an NPC with a
/// viewer skin (`None`: the default skin).
fn build_meta(npc: &Npc, language: &str, own: Option<&Skin>) -> Vec<u8> {
    let mut meta = wire::Meta::new();
    meta.byte(wire::FLAGS, if npc.glow { wire::FLAG_GLOWING } else { 0 });
    let name = npc
        .name_for(language)
        .map(|name| TextComponent::from_legacy_string_with_code(name, '&').encode());
    meta.optional_component(wire::CUSTOM_NAME, name.as_deref());
    meta.boolean(
        wire::CUSTOM_NAME_VISIBLE,
        name.is_some() && npc.name_visible,
    );
    meta.boolean(wire::SILENT, true);
    // Only a lying NPC sends a pose: a standing one keeps the bytes it always had.
    if npc.lie {
        meta.pose(wire::POSE, wire::POSE_SLEEPING);
    }
    if let Kind::Mob(type_name) = &npc.kind
        && npc.baby
        && let Some(index) = wire::baby_index(type_name)
    {
        meta.boolean(index, true);
    }
    if npc.kind == Kind::Player {
        meta.byte(wire::MANNEQUIN_SKIN_PARTS, wire::ALL_SKIN_PARTS);
        meta.boolean(wire::MANNEQUIN_IMMOVABLE, true);
        let skin = if npc.viewer_skin {
            own
        } else {
            npc.skin.as_ref()
        };
        let textures = skin.map(|skin| (skin.value.as_str(), skin.signature.as_deref()));
        let patch = npc.skin_patch.as_ref();
        meta.profile(
            wire::MANNEQUIN_PROFILE,
            &wire::Profile {
                name: None,
                id: Some(npc.uuid),
                textures,
                body: patch.map(|patch| patch.texture.as_str()),
                model: patch.map(|patch| match patch.model {
                    SkinModel::Wide => wire::MODEL_WIDE,
                    SkinModel::Slim => wire::MODEL_SLIM,
                }),
            },
        );
        meta.optional_component(wire::MANNEQUIN_DESCRIPTION, None);
    }
    meta.finish()
}

/// A viewer's own skin, read from the server the first time one of his scans needs it.
struct OwnSkin<'a> {
    player: Option<&'a Player>,
    read: Option<Option<Skin>>,
}

impl<'a> OwnSkin<'a> {
    const fn of(player: &'a Player) -> Self {
        Self {
            player: Some(player),
            read: None,
        }
    }

    /// No player to read from (a test): the default skin.
    #[cfg(test)]
    const fn nobody() -> Self {
        Self {
            player: None,
            read: None,
        }
    }

    /// His skin as the server holds it now (a skin plugin's chosen one too), or `None` for the
    /// default skin: none, or one outside what the client reads.
    fn get(&mut self) -> Option<&Skin> {
        self.read
            .get_or_insert_with(|| {
                self.player
                    .and_then(Player::get_skin)
                    .map(|skin| Skin {
                        from: api::VIEWER_SKIN.into(),
                        value: skin.value,
                        signature: skin.signature,
                    })
                    .filter(model::valid_skin)
            })
            .as_ref()
    }
}

fn meta_packet(
    npc: &Npc,
    language: &'static str,
    cache: &mut BTreeMap<(u32, &'static str), Vec<u8>>,
    own: &mut OwnSkin<'_>,
) -> ClientboundPacket {
    // One entry for an NPC with one name for every language.
    let language = if npc.names.is_empty() { "" } else { language };
    let bytes = if npc.viewer_skin && npc.kind == Kind::Player {
        // Each viewer's own skin: never cached, never sent to anyone else.
        build_meta(npc, language, own.get())
    } else {
        cache
            .entry((npc.num, language))
            .or_insert_with(|| build_meta(npc, language, None))
            .clone()
    };
    ClientboundPacket::CSetEntityMetadata(CSetEntityMetadata {
        entity_id: npc.entity_id(),
        metadata: bytes,
    })
}

/// The size of an NPC that is not the normal one: the `minecraft:scale` attribute, which a packet
/// entity can only get as an update-attributes packet. `None` for the normal size: the client's
/// own default is 1, so nothing is sent (an NPC is always sent whole after a change, `Respawn`).
#[allow(clippy::float_cmp)]
fn scale_packet(npc: &Npc) -> Option<ClientboundPacket> {
    (npc.scale != model::DEFAULT_SCALE).then(|| {
        ClientboundPacket::CUpdateAttributes(CUpdateAttributes {
            entity_id: npc.entity_id(),
            properties: vec![Property {
                id: wire::ATTRIBUTE_SCALE,
                value: f64::from(npc.scale),
                modifiers: Vec::new(),
            }],
        })
    })
}

/// An NPC's own rotation as a turn.
fn home(npc: &Npc) -> Turn {
    let yaw = wire::angle(npc.place.yaw);
    Turn {
        head: yaw,
        body: yaw,
        pitch: wire::angle(npc.place.pitch),
    }
}

fn spawn_packet(npc: &Npc, rot: Option<Turn>) -> ClientboundPacket {
    let (type_id, _) = npc.entity_type();
    let turn = rot.unwrap_or_else(|| home(npc));
    ClientboundPacket::CSpawnEntity(CSpawnEntity {
        entity_id: npc.entity_id(),
        entity_uuid: uuid_of(npc.uuid),
        r_type: type_id,
        position: npc_pos(npc),
        velocity: "[0.0,0.0,0.0]".into(),
        pitch: turn.pitch,
        yaw: turn.body,
        head_yaw: turn.head,
        data: 0,
    })
}

/// The invisible entity a sitting NPC rides.
fn seat_packet(npc: &Npc) -> ClientboundPacket {
    let (type_id, _) = wire::entity_type(model::SEAT_TYPE).unwrap_or((0, 0.0));
    ClientboundPacket::CSpawnEntity(CSpawnEntity {
        entity_id: npc.seat_id(),
        entity_uuid: uuid_of(npc.seat_uuid()),
        r_type: type_id,
        position: (npc.place.x, npc.seat_y(), npc.place.z),
        velocity: "[0.0,0.0,0.0]".into(),
        pitch: 0,
        yaw: 0,
        head_yaw: 0,
        data: 0,
    })
}

/// Sends an NPC to one player, with its seat when it sits.
fn send_npc(
    java: &impl Wire,
    npc: &Npc,
    rot: Option<Turn>,
    language: &'static str,
    cache: &mut BTreeMap<(u32, &'static str), Vec<u8>>,
    own: &mut OwnSkin<'_>,
) {
    if npc.sit {
        send(java, &seat_packet(npc));
    }
    send(java, &spawn_packet(npc, rot));
    send(java, &meta_packet(npc, language, cache, own));
    // The engine has no Bedrock form for an attribute update: a Bedrock viewer sees the normal size.
    if !java.is_bedrock()
        && let Some(packet) = scale_packet(npc)
    {
        send(java, &packet);
    }
    if npc.sit {
        send(
            java,
            &ClientboundPacket::CSetPassengers(CSetPassengers {
                entity_id: npc.seat_id(),
                passengers: vec![npc.entity_id()],
            }),
        );
    }
}

/// Everything a client must lose when the NPC with this number goes: a client ignores an id it
/// does not have, so the seat's id goes along also for an NPC that does not sit.
fn push_ids(ids: &mut Vec<i32>, num: u32) {
    ids.push(model::entity_id(num));
    ids.push(model::seat_id(num));
}

fn forget_meta(cache: &mut BTreeMap<(u32, &'static str), Vec<u8>>, num: u32) {
    cache.retain(|(n, _), _| *n != num);
}

// ---------------------------------------------------------------- the scan

/// Starts the scan chain, or runs the next scan at once (`soon`) after a change.
fn ensure_scan(st: &mut State, soon: bool) {
    if st.scan_running && !soon {
        return;
    }
    let Some(server) = &st.server else {
        return;
    };
    st.scan_gen += 1;
    st.scan_running = true;
    schedule_scan(server, st.scan_gen, 1);
}

fn schedule_scan(server: &Server, generation: u64, delay: u64) {
    server.schedule_delayed_async_task(delay, move |server| scan(&server, generation));
}

fn scan(server: &Server, generation: u64) {
    let next = timed(&SCANS, &SCAN_US, || scan_run(server, generation));
    if let Some(delay) = next {
        schedule_scan(server, generation, delay);
    }
}

fn scan_run(server: &Server, generation: u64) -> Option<u64> {
    with(|st| {
        if st.scan_gen != generation {
            return None;
        }
        let delay = run_scan(st, server);
        if delay.is_none() {
            st.scan_running = false;
        }
        delay
    })
}

/// One scan: every player in a world with NPCs (or with NPCs still on his screen) is sent the NPCs
/// that came into range and loses those that went out. Returns the ticks to the next scan, or
/// `None` when there is nothing to watch.
fn run_scan(st: &mut State, server: &Server) -> Option<u64> {
    let State {
        store,
        viewers,
        refresh,
        removed,
        meta,
        rot,
        look_near,
        ..
    } = st;
    if store.npcs.is_empty() && removed.is_empty() && viewers.values().all(|v| v.visible.is_empty())
    {
        viewers.clear();
        look_near.clear();
        return None;
    }
    let mut worlds: BTreeSet<String> = store.npcs.values().map(|n| n.place.world.clone()).collect();
    worlds.extend(
        viewers
            .values()
            .filter(|v| !v.visible.is_empty())
            .map(|v| v.world.clone()),
    );
    let mut all_ids: Vec<i32> = Vec::with_capacity(store.npcs.len() + removed.len());
    for npc in store.npcs.values() {
        all_ids.push(npc.entity_id());
        if npc.sit {
            all_ids.push(npc.seat_id());
        }
    }
    for num in removed.iter() {
        push_ids(&mut all_ids, *num);
    }
    let mut seen = BTreeSet::new();
    let mut near_any = false;
    let mut budget = SPAWN_BUDGET;
    look_near.clear();
    for world_name in worlds {
        let Some(world) = server.get_world_by_name(&world_name) else {
            continue;
        };
        let here: Vec<&Npc> = store
            .npcs
            .values()
            .filter(|n| n.place.world == world_name)
            .collect();
        let localised = here.iter().any(|n| !n.names.is_empty());
        for player in server.get_players_in_world(world) {
            let key = key_of(&player.get_id());
            let viewer = viewers.entry(key).or_insert_with(|| Viewer {
                reset: true,
                ..Viewer::default()
            });
            let Some(java) = viewer.sink.take().or_else(|| Sink::of(&player)) else {
                continue;
            };
            seen.insert(key);
            let pos = player.get_position();
            if viewer.world != world_name {
                // A world change cleared the client's entities.
                viewer.visible.clear();
                viewer.world.clone_from(&world_name);
            }
            let mut remove: Vec<i32> = Vec::new();
            if viewer.reset {
                remove.extend(&all_ids);
                viewer.visible.clear();
                viewer.reset = false;
                viewer.language = None;
            }
            if localised && viewer.language.is_none() {
                viewer.language = Some(lang::language(&player.get_locale()));
            }
            let language = viewer.language.unwrap_or("en");
            for num in removed.iter() {
                if viewer.visible.remove(num) {
                    push_ids(&mut remove, *num);
                }
            }
            let mut updates = Vec::new();
            if std::mem::take(&mut viewer.language_changed) {
                updates.extend(
                    here.iter()
                        .filter(|n| !n.names.is_empty() && viewer.visible.contains(&n.num))
                        .map(|n| n.num),
                );
            }
            for (num, what) in refresh.iter() {
                if viewer.visible.contains(num) {
                    match what {
                        Refresh::Meta => updates.push(*num),
                        Refresh::Respawn => {
                            viewer.visible.remove(num);
                            push_ids(&mut remove, *num);
                        }
                    }
                }
            }
            let uuid_text = wire::uuid_string(key);
            let mut allowed_nodes: BTreeMap<&str, bool> = BTreeMap::new();
            let mut candidates = Vec::with_capacity(here.len());
            for npc in &here {
                let dist2 = view::dist2(pos, npc_pos(npc));
                let range = f64::from(npc.range);
                near_any |= dist2 <= (range + NEAR_MARGIN) * (range + NEAR_MARGIN);
                let permitted = match npc.permission.as_deref() {
                    None => true,
                    Some(node) => *allowed_nodes
                        .entry(node)
                        .or_insert_with(|| player.has_permission(node)),
                };
                let allowed = permitted && !npc.hidden.contains(&uuid_text);
                if allowed && npc.look && dist2 <= LOOK_FOLLOW * LOOK_FOLLOW {
                    look_near.insert(key);
                }
                candidates.push(Candidate {
                    num: npc.num,
                    dist2,
                    allowed,
                    range,
                });
            }
            let (spawn, despawn) = view::diff(&viewer.visible, &candidates);
            for num in despawn {
                viewer.visible.remove(&num);
                viewer.last_click.remove(&num);
                push_ids(&mut remove, num);
            }
            if !remove.is_empty() {
                remove.sort_unstable();
                remove.dedup();
                send(
                    &java,
                    &ClientboundPacket::CRemoveEntities(CRemoveEntities { entity_ids: remove }),
                );
            }
            let mut own = OwnSkin::of(&player);
            for num in updates {
                if let Some(npc) = here.iter().find(|n| n.num == num) {
                    send(&java, &meta_packet(npc, language, meta, &mut own));
                }
            }
            for num in spawn {
                if budget == 0 {
                    break;
                }
                let Some(npc) = here.iter().find(|n| n.num == num) else {
                    continue;
                };
                budget -= 1;
                send_npc(&java, npc, rot.get(&num).copied(), language, meta, &mut own);
                viewer.visible.insert(num);
            }
            viewer.sink = Some(java);
            viewer.player = Some(player);
        }
    }
    viewers.retain(|key, _| seen.contains(key));
    removed.clear();
    refresh.clear();
    let look_needed = !look_near.is_empty() || !rot.is_empty();
    if look_needed && st.look_task.is_none() {
        st.look_task = Some(server.schedule_repeating_async_task(
            LOOK_PERIOD,
            LOOK_PERIOD,
            |server| look(&server),
        ));
    }
    if budget == 0 {
        return Some(1);
    }
    Some(view::next_delay(near_any))
}

// ---------------------------------------------------------------- looking

/// The head and the body turn of one figure, for one viewer: only what changed.
fn send_turn(sink: &impl Wire, entity_id: i32, turn: Turn, head: bool, body: bool) {
    if head {
        send(
            sink,
            &ClientboundPacket::CHeadRot(CHeadRot {
                entity_id,
                head_yaw: turn.head,
            }),
        );
    }
    if body {
        send(
            sink,
            &ClientboundPacket::CUpdateEntityRot(CUpdateEntityRot {
                entity_id,
                yaw: turn.body,
                pitch: turn.pitch,
                on_ground: true,
            }),
        );
    }
}

/// Turns each looking NPC to the nearest player within [`LOOK_RANGE`], or back to its own
/// rotation; sends only rotations that changed. Stops itself when nobody is near.
fn look(server: &Server) {
    timed(&LOOKS, &LOOK_US, || look_run(server));
}

fn look_run(_server: &Server) {
    with(|st| {
        let State {
            store,
            viewers,
            rot,
            look_near,
            look_task,
            ..
        } = st;
        let mut eyes: BTreeMap<Key, Pos> = BTreeMap::new();
        for key in look_near.iter() {
            if let Some(player) = viewers.get(key).and_then(|v| v.player.as_ref()) {
                let (x, y, z) = player.get_position();
                eyes.insert(*key, (x, y + EYE, z));
            }
        }
        // NPC number, entity id, the turn, and whether the head and the body packet are sent.
        let mut changes: Vec<(u32, i32, Turn, bool, bool)> = Vec::new();
        for npc in store.npcs.values().filter(|n| n.look) {
            let own = home(npc);
            let targets: Vec<Pos> = viewers
                .iter()
                .filter(|(_, v)| v.visible.contains(&npc.num))
                .filter_map(|(key, _)| eyes.get(key).copied())
                .collect();
            let (_, eye) = npc.entity_type();
            let want = match view::nearest(npc_pos(npc), &targets, LOOK_RANGE) {
                Some(i) => {
                    let (yaw, pitch) = view::look_angles(npc_pos(npc), eye, targets[i]);
                    (wire::angle(yaw), wire::angle(pitch))
                }
                None => (own.head, own.pitch),
            };
            let last = rot.get(&npc.num).copied().unwrap_or(own);
            let Some((turn, head, body)) = view::next_turn(last, want.0, want.1, own) else {
                continue;
            };
            changes.push((npc.num, npc.entity_id(), turn, head, body));
            if turn == own {
                rot.remove(&npc.num);
            } else {
                rot.insert(npc.num, turn);
            }
        }
        // Rotations of NPCs that stopped looking or were removed are forgotten.
        rot.retain(|num, _| store.by_num(*num).is_some_and(|n| n.look));
        if !changes.is_empty() {
            for viewer in viewers.values() {
                for (num, entity_id, turn, head, body) in &changes {
                    if !viewer.visible.contains(num) {
                        continue;
                    }
                    let Some(sink) = viewer.sink.as_ref() else {
                        continue;
                    };
                    send_turn(sink, *entity_id, *turn, *head, *body);
                }
            }
        }
        if look_near.is_empty()
            && rot.is_empty()
            && let Some(task) = look_task.take()
        {
            cancel_task(task);
        }
    });
}

// ---------------------------------------------------------------- clicks, actions, menus

fn fill(text: &str, player: &Player) -> String {
    text.replace("{player}", &player.get_name())
}

fn run_action(server: &Server, player: &Player, npc_id: &str, action: &Action) {
    match action {
        Action::Message { text, texts } => {
            // The player's language is read only for a message that has lines per language.
            let line = if texts.is_empty() {
                text.as_str()
            } else {
                model::line_for(text, texts, lang::language(&player.get_locale()))
            };
            player.send_system_message(
                TextComponent::from_legacy_string_with_code(&fill(line, player), '&'),
                false,
            );
        }
        Action::Command(command) | Action::Console(command) => {
            run_click_command(
                npc_id,
                player,
                command,
                matches!(action, Action::Console(_)),
            );
        }
        Action::Teleport(place) => {
            if let Some(world) = server.get_world_by_name(&place.world) {
                player.teleport(
                    (place.x, place.y, place.z),
                    Some(place.yaw),
                    Some(place.pitch),
                    world,
                );
            }
        }
        Action::Menu(name) => open_menu(player, name),
        Action::Ui(file) => open_ui(npc_id, player, file),
    }
}

/// A `command` or `console` action of a click. `dispatch::prepare`
/// decides what runs and refuses on any doubt; the line then runs on the next tick, never inside
/// the click, and only for a player who is still there. At most one command per player in
/// `dispatch::COMMAND_GAP_MS`. A refusal is told to the server's log once per NPC.
fn run_click_command(npc_id: &str, player: &Player, command: &str, console: bool) {
    let run = match dispatch::prepare(console, command, &player.get_name()) {
        Ok(run) => run,
        Err(why) => {
            if with(|st| st.warned.insert(npc_id.to_string())) {
                log(
                    LogLevel::Warn,
                    &format!(
                        "npc-lite: NPC {npc_id} has a command action that ran nothing ({why:?}); it is refused"
                    ),
                );
            }
            return;
        }
    };
    let key = key_of(&player.get_id());
    let now = now_ms();
    let allowed = with(|st| {
        if dispatch::may_run(st.commands_run.get(&key).copied(), now) {
            st.commands_run.insert(key, now);
            true
        } else {
            false
        }
    });
    if !allowed {
        return;
    }
    strawberry_plugin_api::scheduler::schedule_delayed_task(1, move |server| {
        if let Some(player) = server.get_player_by_uuid(uuid_of(key)) {
            dispatch::execute(&server, &player, &run);
        }
    });
}

/// The note a saved command action is followed by: who runs it, and that a console action has
/// every right.
fn saved_note(action: &Action) -> Option<&'static str> {
    match action {
        Action::Command(_) => Some("command_saved"),
        Action::Console(_) => Some("console_saved"),
        _ => None,
    }
}

/// The plugin that shows the server's own menu files.
const UI_PLUGIN: &str = "user_interface";

/// Asks `user_interface` to open the server's file `file` for the player who clicked, as its own
/// command would (`open_file`). That plugin decides: only a file the server owner gave a command,
/// key or item, only for a player with its menu right, at most two a second. On the next tick, so
/// no plugin waits on another inside a click. A refusal is told to the server's log once per NPC.
fn open_ui(npc_id: &str, player: &Player, file: &str) {
    let ask = serde_json::json!({
        "op": "open_file",
        "player": wire::uuid_string(key_of(&player.get_id())),
        "file": file,
    })
    .to_string()
    .into_bytes();
    let npc_id = npc_id.to_string();
    strawberry_plugin_api::scheduler::schedule_delayed_task(1, move |_server| {
        let answer =
            strawberry_plugin_api::wit::strawberry::plugin::ipc::send_ipc_message(UI_PLUGIN, &ask);
        let why = match answer {
            Ok(Ok(bytes)) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
                Ok(reply) if reply.get("ok") == Some(&serde_json::Value::Bool(true)) => return,
                Ok(reply) => reply
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("refused")
                    .to_string(),
                Err(_) => "an answer that is not JSON".to_string(),
            },
            Ok(Err(why)) => why,
            Err(()) => "the user_interface plugin is not on this server".to_string(),
        };
        // A busy player (a menu opened a moment ago) is not the server owner's to fix.
        if why.starts_with("busy:") || why == "player not online" {
            return;
        }
        if with(|st| st.warned.insert(npc_id.clone())) {
            log(
                LogLevel::Warn,
                &format!("npc-lite: NPC {npc_id} has a ui action that opened nothing: {why}"),
            );
        }
    });
}

fn open_menu(player: &Player, name: &str) {
    let key = key_of(&player.get_id());
    let menu = with(|st| {
        let menu = st.store.menus.get(name)?.clone();
        st.viewers.get_mut(&key)?.open_menu = Some(name.to_string());
        Some(menu)
    });
    let (Some(menu), Some(java)) = (menu, player.as_java()) else {
        return;
    };
    let buttons = menu
        .buttons
        .iter()
        .enumerate()
        .map(|(i, button)| dialog::ActionButton {
            text: TextComponent::from_legacy_string_with_code(&button.label, '&'),
            tooltip: None,
            width: None,
            action: dialog::Action::CustomClick(dialog::CustomClickAction {
                id: format!("{DIALOG_PREFIX}{}/{i}", menu.name),
                payload: None,
            }),
        })
        .collect();
    java.show_dialog(dialog::Dialog {
        title: TextComponent::from_legacy_string_with_code(&menu.title, '&'),
        type_: dialog::DialogType::MultiAction,
        body: Vec::new(),
        inputs: Vec::new(),
        buttons,
        links: Vec::new(),
        after_action: None,
        can_close_with_escape: true,
        external_title: None,
    });
}

/// Who clicked, read once per click.
struct Clicker {
    key: Key,
    world: String,
    eye: Pos,
    reach: f64,
}

/// `None` for a spectator: vanilla's spectator interacts with nothing.
fn clicker(player: &Player) -> Option<Clicker> {
    let reach = match player.get_gamemode() {
        GameMode::Spectator => return None,
        GameMode::Creative => 5.0,
        GameMode::Survival | GameMode::Adventure => 3.0,
    } + CLICK_LENIENCY;
    let (x, y, z) = player.get_position();
    Some(Clicker {
        key: key_of(&player.get_id()),
        world: player.get_world().get_name(),
        eye: (x, y + EYE, z),
        reach,
    })
}

/// The checks every click passes, for an action or for an owner plugin that asks: this player was
/// sent this NPC, is in its world and reaches it, one physical click, the cooldown. A click that
/// runs is recorded. `None`: it does not count at all.
fn check_click(st: &mut State, num: u32, who: &Clicker, now: u64) -> Option<ClickVerdict> {
    let npc = st.store.by_num(num)?;
    let viewer = st.viewers.get_mut(&who.key)?;
    let (width, height) = wire::entity_size(model::kind_name(&npc.kind)).unwrap_or((0.6, 1.8));
    if !viewer.visible.contains(&num)
        || npc.place.world != who.world
        || !view::reaches(who.eye, npc_pos(npc), width, height, who.reach)
    {
        return None;
    }
    let verdict = view::click_verdict(viewer.last_click.get(&num).copied(), now, npc.cooldown_ms);
    if verdict == ClickVerdict::Run {
        viewer.last_click.insert(num, now);
    }
    Some(verdict)
}

fn click(server: &Server, player: &Player, entity_id: i32, left: bool) {
    let Some(num) = model::num_of(entity_id) else {
        return;
    };
    let Some(who) = clicker(player) else {
        return;
    };
    let now = now_ms();
    let chosen = with(|st| {
        // A plugin's NPC without actions: the plugin listens to this event itself and asks.
        let npc = st.store.by_num(num)?;
        if npc.owner.is_some() && npc.actions.is_empty() {
            return None;
        }
        let verdict = check_click(st, num, &who, now)?;
        let npc = st.store.by_num(num)?;
        match verdict {
            ClickVerdict::Duplicate => None,
            // A short cooldown waits silently; a long one says how long.
            ClickVerdict::Wait(left) => (npc.cooldown_ms >= 1000).then_some(Err(left)),
            ClickVerdict::Run => {
                let trigger = if left { Trigger::Left } else { Trigger::Right };
                let actions: Vec<Action> = npc
                    .actions
                    .iter()
                    .filter(|(on, _)| *on == Trigger::Any || *on == trigger)
                    .map(|(_, action)| action.clone())
                    .collect();
                Some(Ok((npc.id.clone(), actions)))
            }
        }
    });
    match chosen {
        Some(Ok((npc_id, actions))) => {
            for action in &actions {
                run_action(server, player, &npc_id, action);
            }
        }
        Some(Err(left)) => {
            let seconds = format!("{:.1}", left as f64 / 1000.0);
            let language = lang::language(&player.get_locale());
            let line = lang::text(language, "cooldown_wait", &[&seconds]);
            player.send_system_message(TextComponent::text(&line), false);
        }
        None => {}
    }
}

struct Click;

impl EventHandler<PlayerInteractUnknownEntityEvent> for Click {
    fn handle(
        &self,
        server: Server,
        event: EventData<PlayerInteractUnknownEntityEvent>,
    ) -> EventData<PlayerInteractUnknownEntityEvent> {
        // A click another plugin refused (a login or protection script, or a page open over the
        // world) runs nothing: an action may open a menu whose buttons run console commands.
        if event.cancelled {
            return event;
        }
        click(
            &server,
            &event.player,
            event.entity_id,
            matches!(event.action, EntityInteractionAction::Attack),
        );
        event
    }
}

/// A script was disabled, unloaded or reloaded: the figures it asked for that live in memory only
/// go with it (`persist: true` ones stay). Only on this event, nothing per tick.
struct ScriptDisabled;

impl ExtensionEventHandler for ScriptDisabled {
    fn handle(&self, _server: Server, event: &ExtensionEvent) {
        let Some(plugin) = event.plugin_name() else {
            return;
        };
        // npc-lite's own unload is `on_unload`'s.
        if plugin == PLUGIN_NAME {
            return;
        }
        with(|st| forget_memory_only_of(st, &plugin));
    }
}

/// Takes the memory-only figures of the script `owner` from the store and, at the next scan, from
/// every player who has them. How many.
fn forget_memory_only_of(st: &mut State, owner: &str) -> usize {
    let gone = st.store.remove_memory_only_of(owner);
    if gone.is_empty() {
        return 0;
    }
    for (id, num) in &gone {
        st.removed.insert(*num);
        forget_meta(&mut st.meta, *num);
        st.rot.remove(num);
        st.lookups.retain(|l| &l.npc != id);
    }
    ensure_scan(st, true);
    gone.len()
}

struct MenuClick;

impl EventHandler<DialogClickActionEvent> for MenuClick {
    fn handle(
        &self,
        server: Server,
        event: EventData<DialogClickActionEvent>,
    ) -> EventData<DialogClickActionEvent> {
        let Some((menu, index)) = event
            .id
            .strip_prefix(DIALOG_PREFIX)
            .and_then(|rest| rest.rsplit_once('/'))
        else {
            return event;
        };
        let Ok(index) = index.parse::<usize>() else {
            return event;
        };
        let key = key_of(&event.player.get_id());
        let action = with(|st| {
            let viewer = st.viewers.get_mut(&key)?;
            // Only the menu this player was shown, once.
            if viewer.open_menu.as_deref() != Some(menu) {
                return None;
            }
            viewer.open_menu = None;
            st.store.menus.get(menu)?.buttons.get(index)?.action.clone()
        });
        if let Some(action) = action {
            run_action(&server, &event.player, menu, &action);
        }
        event
    }
}

struct Leave;

impl EventHandler<PlayerLeaveEvent> for Leave {
    fn handle(
        &self,
        _server: Server,
        event: EventData<PlayerLeaveEvent>,
    ) -> EventData<PlayerLeaveEvent> {
        let key = key_of(&event.player.get_id());
        with(|st| {
            st.viewers.remove(&key);
            st.look_near.remove(&key);
            st.commands_run.remove(&key);
        });
        event
    }
}

fn reset_viewer(player: &Player) {
    let key = key_of(&player.get_id());
    with(|st| {
        if let Some(viewer) = st.viewers.get_mut(&key) {
            viewer.reset = true;
        }
    });
}

struct LocaleChange;

impl EventHandler<PlayerLocaleChangeEvent> for LocaleChange {
    fn handle(
        &self,
        _server: Server,
        event: EventData<PlayerLocaleChangeEvent>,
    ) -> EventData<PlayerLocaleChangeEvent> {
        let key = key_of(&event.player.get_id());
        let language = lang::language(&event.new_locale);
        with(|st| {
            if let Some(viewer) = st.viewers.get_mut(&key)
                && viewer.language.is_some()
                && viewer.language != Some(language)
            {
                viewer.language = Some(language);
                viewer.language_changed = true;
            }
        });
        event
    }
}

struct ChangeWorld;

impl EventHandler<PlayerChangeWorldEvent> for ChangeWorld {
    fn handle(
        &self,
        _server: Server,
        event: EventData<PlayerChangeWorldEvent>,
    ) -> EventData<PlayerChangeWorldEvent> {
        reset_viewer(&event.player);
        event
    }
}

struct Respawn;

impl EventHandler<PlayerRespawnEvent> for Respawn {
    fn handle(
        &self,
        _server: Server,
        event: EventData<PlayerRespawnEvent>,
    ) -> EventData<PlayerRespawnEvent> {
        reset_viewer(&event.player);
        event
    }
}

// ---------------------------------------------------------------- skins

fn ensure_lookups(st: &mut State) {
    if st.lookup_task.is_some() || st.lookups.is_empty() {
        return;
    }
    if let Some(server) = &st.server {
        st.lookup_task =
            Some(server.schedule_repeating_async_task(2, 2, |server| poll_lookups(&server)));
    }
}

fn poll_lookups(server: &Server) {
    let done = with(|st| {
        let mut done = Vec::new();
        st.lookups.retain(|lookup| match lookup.job.poll() {
            Some(result) => {
                done.push((
                    lookup.npc.clone(),
                    lookup.account.clone(),
                    lookup.asker,
                    result,
                ));
                false
            }
            None => true,
        });
        if st.lookups.is_empty()
            && let Some(task) = st.lookup_task.take()
        {
            cancel_task(task);
        }
        let mut told = Vec::new();
        for (npc_id, account, asker, result) in done {
            let key = match result {
                Ok(skin) => {
                    let skin = Skin {
                        from: account.clone(),
                        value: skin.value,
                        signature: skin.signature,
                    };
                    if !model::valid_skin(&skin) || st.broken.is_some() {
                        "skin_not_found"
                    } else if let Some(npc) = st.store.npcs.get_mut(&npc_id) {
                        npc.skin = Some(skin);
                        npc.viewer_skin = false;
                        let num = npc.num;
                        changed(st, num, Refresh::Meta);
                        "skin_set"
                    } else {
                        continue;
                    }
                }
                Err(SkinError::NotFound) => "skin_not_found",
                Err(SkinError::Unavailable) => "skin_busy",
            };
            told.push((asker, key, npc_id, account));
        }
        told
    });
    for (asker, key, npc_id, account) in done {
        let Some(player) = asker.and_then(|k| server.get_player_by_uuid(uuid_of(k))) else {
            continue;
        };
        let language = lang::language(&player.get_locale());
        let args: [&str; 2] = if key == "skin_set" {
            [&npc_id, &account]
        } else {
            [&account, ""]
        };
        player.send_system_message(
            TextComponent::text(&lang::text(language, key, &args)),
            false,
        );
    }
}

// ---------------------------------------------------------------- commands

/// Marks an NPC changed: its entity data is built again and every viewer gets it.
fn changed(st: &mut State, num: u32, what: Refresh) {
    forget_meta(&mut st.meta, num);
    let entry = st.refresh.entry(num).or_insert(what);
    if what == Refresh::Respawn {
        *entry = Refresh::Respawn;
    }
    if st.store.by_num(num).is_none_or(|npc| npc.persist) {
        ensure_save(st);
    }
    ensure_scan(st, true);
}

fn language_of(sender: &CommandSender) -> &'static str {
    match sender.as_player() {
        Some(player) => lang::language(&player.get_locale()),
        None => lang::language(&format!("{:?}", sender.get_locale())),
    }
}

fn say(sender: &CommandSender, language: &str, key: &str, args: &[&str]) {
    sender.send_message(TextComponent::text(&lang::text(language, key, args)));
}

fn fail(language: &str, key: &str, args: &[&str]) -> CommandError {
    CommandError::CommandFailed(TextComponent::text(&lang::text(language, key, args)))
}

fn word(args: &ConsumedArgs, name: &str) -> Option<String> {
    match args.get_value(name) {
        // A missing argument reads as an empty text.
        Arg::Simple(text) | Arg::Msg(text) if !text.is_empty() => Some(text),
        _ => None,
    }
}

fn flag(args: &ConsumedArgs, name: &str) -> Option<bool> {
    match args.get_value(name) {
        Arg::Bool(value) => Some(value),
        _ => None,
    }
}

/// The `size` of `/npc scale`: a number of any width, as the game's attribute holds it.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn size_arg(args: &ConsumedArgs) -> Option<f32> {
    match args.get_value("size") {
        Arg::Num(Ok(Number::Float64(v))) => Some(v as f32),
        Arg::Num(Ok(Number::Float32(v))) => Some(v),
        Arg::Num(Ok(Number::Int32(v))) => Some(v as f32),
        Arg::Num(Ok(Number::Int64(v))) => Some(v as f32),
        _ => None,
    }
}

fn index(args: &ConsumedArgs, name: &str) -> Option<usize> {
    match args.get_value(name) {
        Arg::Num(Ok(Number::Int32(value))) => usize::try_from(value).ok(),
        Arg::Num(Ok(Number::Int64(value))) => usize::try_from(value).ok(),
        _ => None,
    }
}

fn place_of(player: &Player) -> Place {
    let (x, y, z) = player.get_position();
    Place {
        world: player.get_world().get_name(),
        x,
        y,
        z,
        yaw: player.get_yaw(),
        pitch: player.get_pitch().clamp(-90.0, 90.0),
    }
}

#[derive(Clone, Copy)]
enum Op {
    Usage,
    List,
    Reload,
    Create,
    Remove,
    Info,
    MoveHere,
    Name,
    NameClear,
    Skin,
    SkinClear,
    Texture,
    TextureClear,
    Type,
    Glow,
    Look,
    Permission,
    PermissionClear,
    Hide,
    Show,
    ActionAdd,
    ActionRemove,
    ActionClear,
    MenuList,
    MenuCreate,
    MenuRemove,
    MenuInfo,
    MenuButton,
    MenuAction,
    Stats,
    Cooldown,
    Range,
    Scale,
    Baby,
    NameVisible,
    Sit,
    Lie,
    NameFor,
    NameForClear,
    Import,
}

struct Cmd(Op);

impl CommandHandler for Cmd {
    fn handle(
        &self,
        sender: CommandSender,
        server: Server,
        args: ConsumedArgs,
    ) -> std::result::Result<i32, CommandError> {
        let language = language_of(&sender);
        run_command(self.0, &sender, &server, &args, language)
    }
}

type Reply = std::result::Result<i32, CommandError>;

fn yes_no(language: &str, value: bool) -> String {
    lang::text(language, if value { "yes" } else { "no" }, &[])
}

fn run_command(
    op: Op,
    sender: &CommandSender,
    server: &Server,
    args: &ConsumedArgs,
    language: &str,
) -> Reply {
    let id = word(args, "id").unwrap_or_default();
    match op {
        Op::Usage => {
            for key in [
                "usage_title",
                "usage_create",
                "usage_edit",
                "usage_view",
                "usage_action",
                "usage_console",
                "usage_menu",
                "usage_other",
            ] {
                say(sender, language, key, &[]);
            }
            Ok(1)
        }
        Op::List => {
            let names: Vec<String> = with(|st| st.store.npcs.keys().cloned().collect());
            if names.is_empty() {
                say(sender, language, "list_empty", &[]);
            } else {
                say(
                    sender,
                    language,
                    "list",
                    &[&names.len().to_string(), &names.join(", ")],
                );
            }
            Ok(i32::try_from(names.len()).unwrap_or(i32::MAX))
        }
        Op::MenuList => {
            let names: Vec<String> = with(|st| st.store.menus.keys().cloned().collect());
            if names.is_empty() {
                say(sender, language, "menus_empty", &[]);
            } else {
                say(
                    sender,
                    language,
                    "menus",
                    &[&names.len().to_string(), &names.join(", ")],
                );
            }
            Ok(i32::try_from(names.len()).unwrap_or(i32::MAX))
        }
        Op::Reload => match load() {
            Ok(store) => {
                let (npcs, menus) = (store.npcs.len(), store.menus.len());
                with(|st| {
                    let old = std::mem::replace(&mut st.store, store);
                    st.removed.extend(old.npcs.values().map(|n| n.num));
                    // What plugins keep in memory only is not in the file.
                    st.store.adopt_memory_only(old);
                    st.broken = None;
                    st.dirty = false;
                    st.meta.clear();
                    st.rot.clear();
                    for viewer in st.viewers.values_mut() {
                        viewer.reset = true;
                    }
                    ensure_scan(st, true);
                });
                say(
                    sender,
                    language,
                    "reloaded",
                    &[&npcs.to_string(), &menus.to_string()],
                );
                Ok(1)
            }
            Err(reason) => Err(fail(language, "reload_failed", &[&reason])),
        },
        Op::Info => info(sender, language, &id),
        Op::Stats => {
            let ms = |us: &AtomicU64| format!("{:.1}", us.load(Relaxed) as f64 / 1000.0);
            let values = [
                SCANS.load(Relaxed).to_string(),
                ms(&SCAN_US),
                LOOKS.load(Relaxed).to_string(),
                ms(&LOOK_US),
                PACKETS.load(Relaxed).to_string(),
            ];
            let refs: Vec<&str> = values.iter().map(String::as_str).collect();
            say(sender, language, "stats", &refs);
            Ok(1)
        }
        Op::MenuInfo => {
            let menu_name = word(args, "menu").unwrap_or_default();
            let Some(menu) = with(|st| st.store.menus.get(&menu_name).cloned()) else {
                return Err(fail(language, "menu_unknown", &[&menu_name]));
            };
            say(sender, language, "menu_info", &[&menu.name, &menu.title]);
            if menu.buttons.is_empty() {
                say(sender, language, "menu_no_buttons", &[&menu.name]);
            }
            for (i, button) in menu.buttons.iter().enumerate() {
                let action = button
                    .action
                    .as_ref()
                    .map_or_else(|| lang::text(language, "nothing", &[]), model::action_text);
                say(
                    sender,
                    language,
                    "menu_button",
                    &[&(i + 1).to_string(), &button.label, &action],
                );
            }
            Ok(1)
        }
        _ => edit(op, sender, server, args, language, &id),
    }
}

fn info(sender: &CommandSender, language: &str, id: &str) -> Reply {
    let Some(npc) = with(|st| st.store.npcs.get(id).cloned()) else {
        return Err(fail(language, "unknown", &[id]));
    };
    let place = format!("{:.2} {:.2} {:.2}", npc.place.x, npc.place.y, npc.place.z);
    say(
        sender,
        language,
        "info_where",
        &[
            &npc.id,
            model::kind_name(&npc.kind),
            &place,
            &npc.place.world,
        ],
    );
    if let Some(owner) = &npc.owner {
        say(sender, language, "info_owner", &[owner]);
    }
    match &npc.name {
        Some(name) => say(sender, language, "info_name", &[name]),
        None => say(sender, language, "info_no_name", &[]),
    }
    if !npc.names.is_empty() {
        let names: Vec<String> = npc
            .names
            .iter()
            .map(|(code, text)| format!("{code}: {text}"))
            .collect();
        say(sender, language, "info_names", &[&names.join(", ")]);
    }
    if npc.kind == Kind::Player {
        match &npc.skin {
            _ if npc.viewer_skin => say(sender, language, "info_viewer_skin", &[]),
            Some(skin) => say(sender, language, "info_skin", &[&skin.from]),
            None => say(sender, language, "info_default_skin", &[]),
        }
        if let Some(patch) = &npc.skin_patch {
            say(
                sender,
                language,
                "info_texture",
                &[&patch.texture, model::model_name(patch.model)],
            );
        }
    }
    say(
        sender,
        language,
        "info_flags",
        &[
            &yes_no(language, npc.glow),
            &yes_no(language, npc.look),
            &yes_no(language, npc.baby),
            &yes_no(language, npc.sit),
            &yes_no(language, npc.lie),
        ],
    );
    say(
        sender,
        language,
        "info_view",
        &[&npc.range.to_string(), &npc.cooldown_ms.to_string()],
    );
    if (npc.scale - model::DEFAULT_SCALE).abs() > f32::EPSILON {
        say(sender, language, "info_scale", &[&npc.scale.to_string()]);
    }
    match &npc.permission {
        Some(node) => say(sender, language, "info_permission", &[node]),
        None => say(sender, language, "info_everyone", &[]),
    }
    if !npc.hidden.is_empty() {
        say(
            sender,
            language,
            "info_hidden",
            &[&npc.hidden.len().to_string()],
        );
    }
    if npc.actions.is_empty() {
        say(sender, language, "info_no_actions", &[]);
    }
    for (i, (on, action)) in npc.actions.iter().enumerate() {
        say(
            sender,
            language,
            "info_action",
            &[
                &(i + 1).to_string(),
                model::trigger_name(*on),
                &model::action_text(action),
            ],
        );
    }
    Ok(1)
}

/// Every command that changes something: refused while the data file cannot be read.
#[expect(clippy::too_many_lines, reason = "one arm per subcommand")]
fn edit(
    op: Op,
    sender: &CommandSender,
    server: &Server,
    args: &ConsumedArgs,
    language: &str,
    id: &str,
) -> Reply {
    if let Some(reason) = with(|st| st.broken.clone()) {
        return Err(fail(language, "data_bad", &[&reason]));
    }
    let player = sender.as_player();
    match op {
        Op::Create => {
            let Some(player) = player else {
                return Err(fail(language, "players_only", &[]));
            };
            if !model::valid_id(id) {
                return Err(fail(language, "bad_id", &[]));
            }
            let type_name = word(args, "type").unwrap_or_else(|| "player".into());
            let Some(kind) = model::parse_kind(&type_name) else {
                return Err(fail(language, "bad_type", &[&type_name]));
            };
            let place = place_of(&player);
            let uuid = strawberry_plugin_api::uuid::generate();
            let result = with(|st| {
                let result = st
                    .store
                    .create(id, kind, place, key_of(&uuid))
                    .map(|npc| npc.num);
                if result.is_ok() {
                    ensure_save(st);
                    ensure_scan(st, true);
                }
                result
            });
            match result {
                Ok(_) => {
                    say(sender, language, "created", &[id]);
                    Ok(1)
                }
                Err(CreateError::BadId) => Err(fail(language, "bad_id", &[])),
                Err(CreateError::Exists) => Err(fail(language, "exists", &[id])),
                Err(CreateError::Limit) => {
                    Err(fail(language, "limit", &[&model::MAX_NPCS.to_string()]))
                }
            }
        }
        Op::Remove => {
            let removed = with(|st| {
                let npc = st.store.npcs.remove(id)?;
                st.removed.insert(npc.num);
                forget_meta(&mut st.meta, npc.num);
                st.rot.remove(&npc.num);
                st.lookups.retain(|l| l.npc != id);
                if npc.persist {
                    ensure_save(st);
                }
                ensure_scan(st, true);
                Some(())
            });
            if removed.is_none() {
                return Err(fail(language, "unknown", &[id]));
            }
            say(sender, language, "removed", &[id]);
            Ok(1)
        }
        Op::Import => run_import(sender, language, console_right(sender, server)),
        Op::MenuCreate => {
            let menu_name = word(args, "menu").unwrap_or_default();
            let title = word(args, "title").unwrap_or_default();
            if !model::valid_id(&menu_name) {
                return Err(fail(language, "bad_id", &[]));
            }
            if !model::valid_text(&title, model::MAX_NAME) {
                return Err(fail(language, "bad_text", &[&model::MAX_NAME.to_string()]));
            }
            let result = with(|st| {
                if st.store.menus.contains_key(&menu_name) {
                    return Err("menu_exists");
                }
                if st.store.menus.len() >= model::MAX_MENUS {
                    return Err("menu_limit");
                }
                st.store.menus.insert(
                    menu_name.clone(),
                    Menu {
                        name: menu_name.clone(),
                        title,
                        buttons: Vec::new(),
                    },
                );
                ensure_save(st);
                Ok(())
            });
            match result {
                Ok(()) => {
                    say(sender, language, "menu_created", &[&menu_name]);
                    Ok(1)
                }
                Err("menu_limit") => Err(fail(
                    language,
                    "menu_limit",
                    &[&model::MAX_MENUS.to_string()],
                )),
                Err(key) => Err(fail(language, key, &[&menu_name])),
            }
        }
        Op::MenuRemove => {
            let menu_name = word(args, "menu").unwrap_or_default();
            let removed = with(|st| {
                let found = st.store.menus.remove(&menu_name).is_some();
                if found {
                    ensure_save(st);
                }
                found
            });
            if !removed {
                return Err(fail(language, "menu_unknown", &[&menu_name]));
            }
            say(sender, language, "menu_removed", &[&menu_name]);
            Ok(1)
        }
        Op::MenuButton => {
            let menu_name = word(args, "menu").unwrap_or_default();
            let label = word(args, "label").unwrap_or_default();
            if !model::valid_text(&label, model::MAX_NAME) {
                return Err(fail(language, "bad_text", &[&model::MAX_NAME.to_string()]));
            }
            let result = with(|st| {
                let menu = st.store.menus.get_mut(&menu_name).ok_or("menu_unknown")?;
                if menu.buttons.len() >= model::MAX_BUTTONS {
                    return Err("button_limit");
                }
                menu.buttons.push(Button {
                    label,
                    action: None,
                });
                let number = menu.buttons.len();
                ensure_save(st);
                Ok(number)
            });
            match result {
                Ok(number) => {
                    say(
                        sender,
                        language,
                        "button_added",
                        &[&menu_name, &number.to_string()],
                    );
                    Ok(1)
                }
                Err("button_limit") => Err(fail(
                    language,
                    "button_limit",
                    &[&menu_name, &model::MAX_BUTTONS.to_string()],
                )),
                Err(key) => Err(fail(language, key, &[&menu_name])),
            }
        }
        Op::MenuAction => {
            let menu_name = word(args, "menu").unwrap_or_default();
            let number = index(args, "index").unwrap_or(0);
            let here = player.as_ref().map(place_of);
            let action =
                parse_action_args(args, here.as_ref(), language, console_right(sender, server))?;
            let shown = model::action_text(&action);
            let saved = saved_note(&action);
            let result = with(|st| {
                let menu = st.store.menus.get_mut(&menu_name).ok_or("menu_unknown")?;
                let button = number
                    .checked_sub(1)
                    .and_then(|i| menu.buttons.get_mut(i))
                    .ok_or("no_button")?;
                button.action = Some(action);
                ensure_save(st);
                Ok(())
            });
            match result {
                Ok(()) => {
                    say(
                        sender,
                        language,
                        "button_action",
                        &[&menu_name, &number.to_string(), &shown],
                    );
                    if let Some(key) = saved {
                        say(sender, language, key, &[]);
                    }
                    Ok(1)
                }
                Err("no_button") => Err(fail(
                    language,
                    "no_button",
                    &[&menu_name, &number.to_string()],
                )),
                Err(key) => Err(fail(language, key, &[&menu_name])),
            }
        }
        _ => edit_npc(op, sender, server, args, language, id, player.as_ref()),
    }
}

/// One line for a row that was not imported.
fn skipped_text(language: &str, why: &import::Skipped) -> String {
    match why {
        import::Skipped::WorldIdOnly => lang::text(language, "skip_world_id", &[]),
        import::Skipped::NotShown => lang::text(language, "skip_not_shown", &[]),
        import::Skipped::BadPlace => lang::text(language, "skip_place", &[]),
        import::Skipped::UnknownType(name) => lang::text(language, "bad_type", &[name]),
    }
}

fn dropped_text(language: &str, what: &import::Dropped) -> String {
    match what {
        import::Dropped::Equipment => lang::text(language, "drop_equipment", &[]),
        import::Dropped::VanillaAi => lang::text(language, "drop_ai", &[]),
        import::Dropped::ClickType => lang::text(language, "drop_click", &[]),
        import::Dropped::Command => lang::text(language, "drop_command", &[]),
        import::Dropped::SkinByName => lang::text(language, "drop_skin", &[]),
        import::Dropped::Part(part) => part.clone(),
    }
}

/// `/npc import`: every row of `data/import.yml` that has no NPC of its name yet becomes one. One
/// bounded read on request, then the normal save off the tick.
fn run_import(sender: &CommandSender, language: &str, console_right: bool) -> Reply {
    match std::fs::metadata(IMPORT_FILE) {
        Err(_) => return Err(fail(language, "import_missing", &[])),
        Ok(meta) if meta.len() > import::MAX_IMPORT as u64 => {
            return Err(fail(
                language,
                "import_bad",
                &[&format!("larger than {} bytes", import::MAX_IMPORT)],
            ));
        }
        Ok(_) => {}
    }
    let mut report = std::fs::read_to_string(IMPORT_FILE)
        .map_err(|e| e.to_string())
        .and_then(|text| import::read_saves(&text))
        .map_err(|reason| fail(language, "import_bad", &[&reason]))?;
    if !console_right {
        import::drop_console_actions(&mut report.rows);
    }
    let mut lines: Vec<String> = report
        .skipped
        .iter()
        .map(|(number, why)| {
            lang::text(
                language,
                "import_skip",
                &[number, &skipped_text(language, why)],
            )
        })
        .collect();
    let mut made = 0;
    for row in report.rows {
        let uuid = strawberry_plugin_api::uuid::generate();
        let id = row.id.clone();
        let outcome = with(|st| {
            st.store
                .create(&id, row.kind.clone(), row.place.clone(), key_of(&uuid))?;
            if let Some(npc) = st.store.npcs.get_mut(&id) {
                npc.name.clone_from(&row.name);
                npc.name_visible = row.name_visible;
                npc.skin.clone_from(&row.skin);
                npc.glow = row.glow;
                npc.look = row.look;
                npc.baby = row.baby;
                npc.sit = row.sit;
                npc.actions.clone_from(&row.actions);
            }
            Ok(())
        });
        match outcome {
            Ok(()) => {
                made += 1;
                if !row.dropped.is_empty() {
                    let lost: Vec<String> = row
                        .dropped
                        .iter()
                        .map(|what| dropped_text(language, what))
                        .collect();
                    lines.push(lang::text(
                        language,
                        "import_lost",
                        &[&id, &lost.join(", ")],
                    ));
                }
            }
            Err(CreateError::Exists) => {
                lines.push(lang::text(language, "exists", &[&id]));
            }
            Err(CreateError::BadId) => {
                lines.push(lang::text(language, "bad_id", &[]));
            }
            Err(CreateError::Limit) => {
                lines.push(lang::text(
                    language,
                    "limit",
                    &[&model::MAX_NPCS.to_string()],
                ));
                break;
            }
        }
    }
    if made > 0 {
        with(|st| {
            ensure_save(st);
            ensure_scan(st, true);
        });
    }
    let more = lines.len().saturating_sub(IMPORT_LINES);
    for line in lines.iter().take(IMPORT_LINES) {
        sender.send_message(TextComponent::text(line));
    }
    if more > 0 {
        say(sender, language, "import_more", &[&more.to_string()]);
    }
    say(
        sender,
        language,
        "imported",
        &[&made.to_string(), &report.skipped.len().to_string()],
    );
    Ok(made)
}

/// Whether the sender may store or change a `console` action (`dispatch::may_store`).
fn console_right(sender: &CommandSender, server: &Server) -> bool {
    sender.is_console() || sender.has_permission(server, CONSOLE_NODE)
}

fn parse_action_args(
    args: &ConsumedArgs,
    here: Option<&Place>,
    language: &str,
    console_right: bool,
) -> std::result::Result<Action, CommandError> {
    let kind = word(args, "kind").unwrap_or_default();
    let value = word(args, "value").unwrap_or_default();
    let action = model::parse_action(&kind, &value, here).map_err(|e| match e {
        model::ActionError::UnknownType => fail(language, "bad_action_type", &[]),
        model::ActionError::BadText => {
            fail(language, "bad_text", &[&model::MAX_MESSAGE.to_string()])
        }
        model::ActionError::BadPlace => fail(language, "bad_place", &[]),
        model::ActionError::BadMenu => fail(language, "menu_unknown", &[&value]),
        model::ActionError::BadUi => fail(language, "bad_ui", &[]),
    })?;
    if !dispatch::may_store(matches!(action, Action::Console(_)), console_right) {
        return Err(fail(language, "console_right", &[CONSOLE_NODE]));
    }
    // A stored command is judged when it is made, so the server owner is told at once; it is
    // judged again at every click.
    let stored = match &action {
        Action::Command(command) => Some((command.as_str(), false)),
        Action::Console(command) => Some((command.as_str(), true)),
        _ => None,
    };
    if let Some((command, console)) = stored {
        dispatch::check_stored(console, command).map_err(|why| {
            let root = command_rules::root(command).unwrap_or_default();
            match why {
                dispatch::Refused::Platform => fail(language, "command_platform", &[&root]),
                dispatch::Refused::ForbiddenFromConsole => {
                    fail(language, "command_console", &[&root])
                }
                dispatch::Refused::NoPlainName => fail(language, "command_name", &[]),
                _ => fail(language, "bad_text", &[&model::MAX_COMMAND.to_string()]),
            }
        })?;
    }
    Ok(action)
}

#[expect(clippy::too_many_lines, reason = "one arm per subcommand")]
fn edit_npc(
    op: Op,
    sender: &CommandSender,
    server: &Server,
    args: &ConsumedArgs,
    language: &str,
    id: &str,
    player: Option<&Player>,
) -> Reply {
    match with(|st| st.store.npcs.get(id).map(|npc| npc.owner.clone())) {
        None => return Err(fail(language, "unknown", &[id])),
        // Only `/npc remove` reaches a plugin's NPC: a server owner can clear what a plugin left.
        Some(Some(owner)) => return Err(fail(language, "owned", &[id, &owner])),
        Some(None) => {}
    }
    // A change applied to the NPC: the reply key and its arguments, or the refusal.
    type Change = std::result::Result<(Option<Refresh>, &'static str, Vec<String>), CommandError>;
    let apply = |f: &dyn Fn(&mut Npc) -> Change| -> Reply {
        let outcome = with(|st| {
            let npc = st.store.npcs.get_mut(id)?;
            let outcome = f(npc);
            let num = npc.num;
            if let Ok((what, _, _)) = &outcome {
                match what {
                    Some(what) => changed(st, num, *what),
                    None => {
                        ensure_save(st);
                        ensure_scan(st, true);
                    }
                }
            }
            Some(outcome)
        });
        let Some(outcome) = outcome else {
            return Err(fail(language, "unknown", &[id]));
        };
        let (_, key, values) = outcome?;
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        say(sender, language, key, &refs);
        Ok(1)
    };
    let id_text = id.to_string();
    match op {
        Op::MoveHere => {
            let Some(player) = player else {
                return Err(fail(language, "players_only", &[]));
            };
            let place = place_of(player);
            with(|st| {
                if let Some(npc) = st.store.npcs.get(id) {
                    st.rot.remove(&npc.num);
                }
            });
            apply(&|npc| {
                npc.place = place.clone();
                Ok((Some(Refresh::Respawn), "moved", vec![id_text.clone()]))
            })
        }
        Op::Name => {
            let text = word(args, "text").unwrap_or_default();
            if !model::valid_text(&text, model::MAX_NAME) {
                return Err(fail(language, "bad_text", &[&model::MAX_NAME.to_string()]));
            }
            apply(&|npc| {
                npc.name = Some(text.clone());
                Ok((
                    Some(Refresh::Meta),
                    "name_set",
                    vec![id_text.clone(), text.clone()],
                ))
            })
        }
        Op::NameClear => apply(&|npc| {
            npc.name = None;
            npc.names.clear();
            Ok((Some(Refresh::Meta), "name_cleared", vec![id_text.clone()]))
        }),
        Op::NameFor | Op::NameForClear => {
            let code = word(args, "language").unwrap_or_default();
            if !model::LANGUAGES.contains(&code.as_str()) {
                return Err(fail(language, "bad_language", &[]));
            }
            if matches!(op, Op::NameForClear) {
                return apply(&|npc| {
                    npc.names.remove(&code);
                    Ok((
                        Some(Refresh::Meta),
                        "name_for_cleared",
                        vec![id_text.clone(), code.clone()],
                    ))
                });
            }
            let text = word(args, "text").unwrap_or_default();
            if !model::valid_text(&text, model::MAX_NAME) {
                return Err(fail(language, "bad_text", &[&model::MAX_NAME.to_string()]));
            }
            apply(&|npc| {
                npc.names.insert(code.clone(), text.clone());
                Ok((
                    Some(Refresh::Meta),
                    "name_for_set",
                    vec![id_text.clone(), code.clone(), text.clone()],
                ))
            })
        }
        Op::Sit | Op::Lie => {
            let value = flag(args, "value").unwrap_or(false);
            let sit = matches!(op, Op::Sit);
            apply(&|npc| {
                // An NPC sits or lies, not both: the other one goes off first.
                if value && (if sit { npc.lie } else { npc.sit }) {
                    return Err(fail(language, "sit_and_lie", &[&id_text]));
                }
                if sit {
                    npc.sit = value;
                } else {
                    npc.lie = value;
                }
                let key = match (sit, value) {
                    (true, true) => "sit_on",
                    (true, false) => "sit_off",
                    (false, true) => "lie_on",
                    (false, false) => "lie_off",
                };
                // Sent again whole: entity data that leaves the pose out would not make a lying
                // NPC stand up on a client that has it.
                Ok((Some(Refresh::Respawn), key, vec![id_text.clone()]))
            })
        }
        Op::Skin => {
            let account = word(args, "account").unwrap_or_default();
            if account == api::VIEWER_SKIN {
                if with(|st| {
                    st.store
                        .npcs
                        .get(id)
                        .is_some_and(|n| n.kind != Kind::Player)
                }) {
                    return Err(fail(language, "skin_players_only", &[]));
                }
                // A lookup still running for this NPC would put an account's skin over it.
                with(|st| st.lookups.retain(|l| l.npc != id));
                return apply(&|npc| {
                    npc.viewer_skin = true;
                    npc.skin = None;
                    Ok((
                        Some(Refresh::Meta),
                        "skin_viewer_set",
                        vec![id_text.clone()],
                    ))
                });
            }
            if !model::valid_account(&account) {
                return Err(fail(language, "bad_account", &[]));
            }
            if with(|st| {
                st.store
                    .npcs
                    .get(id)
                    .is_some_and(|n| n.kind != Kind::Player)
            }) {
                return Err(fail(language, "skin_players_only", &[]));
            }
            // A player online now: his skin as the server holds it (also an offline-mode
            // server's chosen skin).
            if let Some(skin) = server
                .get_player_by_name(&account)
                .and_then(|p| p.get_skin())
            {
                let skin = Skin {
                    from: account.clone(),
                    value: skin.value,
                    signature: skin.signature,
                };
                if model::valid_skin(&skin) {
                    return apply(&|npc| {
                        npc.skin = Some(skin.clone());
                        npc.viewer_skin = false;
                        Ok((
                            Some(Refresh::Meta),
                            "skin_set",
                            vec![id_text.clone(), account.clone()],
                        ))
                    });
                }
            }
            let asker = player.map(|p| key_of(&p.get_id()));
            with(|st| {
                st.lookups.retain(|l| l.npc != id);
                st.lookups.push(Lookup {
                    npc: id.to_string(),
                    account: account.clone(),
                    job: lookup(&account),
                    asker,
                });
                ensure_lookups(st);
            });
            say(sender, language, "skin_looking", &[&account]);
            Ok(1)
        }
        Op::SkinClear => apply(&|npc| {
            npc.skin = None;
            npc.viewer_skin = false;
            Ok((Some(Refresh::Meta), "skin_cleared", vec![id_text.clone()]))
        }),
        Op::Texture => {
            // `namespace:path`, then `wide` or `slim` if wanted.
            let text = word(args, "texture").unwrap_or_default();
            let mut words = text.split_whitespace();
            let texture = words.next().unwrap_or_default().to_string();
            let model = words
                .next()
                .map_or(Some(SkinModel::Wide), model::parse_model);
            let (Some(model), None, true) = (model, words.next(), model::valid_texture(&texture))
            else {
                return Err(fail(language, "bad_texture", &[]));
            };
            if with(|st| {
                st.store
                    .npcs
                    .get(id)
                    .is_some_and(|n| n.kind != Kind::Player)
            }) {
                return Err(fail(language, "skin_players_only", &[]));
            }
            apply(&|npc| {
                npc.skin_patch = Some(SkinPatch {
                    texture: texture.clone(),
                    model,
                });
                Ok((
                    Some(Refresh::Meta),
                    "texture_set",
                    vec![
                        id_text.clone(),
                        texture.clone(),
                        model::model_name(model).to_string(),
                    ],
                ))
            })
        }
        Op::TextureClear => apply(&|npc| {
            npc.skin_patch = None;
            Ok((
                Some(Refresh::Meta),
                "texture_cleared",
                vec![id_text.clone()],
            ))
        }),
        Op::Type => {
            let type_name = word(args, "type").unwrap_or_default();
            let Some(kind) = model::parse_kind(&type_name) else {
                return Err(fail(language, "bad_type", &[&type_name]));
            };
            apply(&|npc| {
                npc.kind = kind.clone();
                Ok((
                    Some(Refresh::Respawn),
                    "type_set",
                    vec![id_text.clone(), model::kind_name(&kind).to_string()],
                ))
            })
        }
        Op::Cooldown => {
            let ms = index(args, "ms")
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(model::DEFAULT_COOLDOWN_MS);
            apply(&|npc| {
                npc.cooldown_ms = ms.min(model::MAX_COOLDOWN_MS);
                Ok((
                    None,
                    "cooldown_set",
                    vec![id_text.clone(), npc.cooldown_ms.to_string()],
                ))
            })
        }
        Op::Range => {
            let blocks = index(args, "blocks")
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(model::DEFAULT_RANGE);
            if !model::valid_range(blocks) {
                return Err(fail(
                    language,
                    "bad_range",
                    &[&model::MIN_RANGE.to_string(), &model::MAX_RANGE.to_string()],
                ));
            }
            apply(&|npc| {
                npc.range = blocks;
                Ok((None, "range_set", vec![id_text.clone(), blocks.to_string()]))
            })
        }
        Op::Scale => {
            let size = size_arg(args);
            let Some(size) = size.filter(|size| model::valid_scale(*size)) else {
                return Err(fail(
                    language,
                    "bad_scale",
                    &[&model::MIN_SCALE.to_string(), &model::MAX_SCALE.to_string()],
                ));
            };
            apply(&|npc| {
                npc.scale = size;
                // Sent again whole: a client that has the NPC keeps no other way to go back to 1.
                Ok((
                    Some(Refresh::Respawn),
                    "scale_set",
                    vec![id_text.clone(), size.to_string()],
                ))
            })
        }
        Op::Baby => {
            let value = flag(args, "value").unwrap_or(false);
            apply(&|npc| {
                if value && !model::can_be_baby(&npc.kind) {
                    return Err(fail(
                        language,
                        "baby_not_possible",
                        &[model::kind_name(&npc.kind)],
                    ));
                }
                npc.baby = value;
                Ok((
                    Some(Refresh::Meta),
                    if value { "baby_on" } else { "baby_off" },
                    vec![id_text.clone()],
                ))
            })
        }
        Op::NameVisible => {
            let value = flag(args, "value").unwrap_or(true);
            apply(&|npc| {
                npc.name_visible = value;
                Ok((
                    Some(Refresh::Meta),
                    if value {
                        "namevisible_on"
                    } else {
                        "namevisible_off"
                    },
                    vec![id_text.clone()],
                ))
            })
        }
        Op::Glow => {
            let value = flag(args, "value").unwrap_or(false);
            apply(&|npc| {
                npc.glow = value;
                Ok((
                    Some(Refresh::Meta),
                    if value { "glow_on" } else { "glow_off" },
                    vec![id_text.clone()],
                ))
            })
        }
        Op::Look => {
            let value = flag(args, "value").unwrap_or(false);
            apply(&|npc| {
                npc.look = value;
                Ok((
                    Some(Refresh::Respawn),
                    if value { "look_on" } else { "look_off" },
                    vec![id_text.clone()],
                ))
            })
        }
        Op::Permission => {
            let node = word(args, "node").unwrap_or_default().to_ascii_lowercase();
            if !model::valid_node(&node) {
                return Err(fail(language, "bad_node", &[]));
            }
            apply(&|npc| {
                npc.permission = Some(node.clone());
                Ok((None, "permission_set", vec![id_text.clone(), node.clone()]))
            })
        }
        Op::PermissionClear => apply(&|npc| {
            npc.permission = None;
            Ok((None, "permission_cleared", vec![id_text.clone()]))
        }),
        Op::Hide | Op::Show => {
            let Arg::Players(players) = args.get_value("players") else {
                return Err(fail(language, "players_only", &[]));
            };
            let hide = matches!(op, Op::Hide);
            let mut count = 0;
            for target in &players {
                let uuid = wire::uuid_string(key_of(&target.get_id()));
                let name = target.get_name();
                apply(&|npc| {
                    if hide {
                        if npc.hidden.len() >= model::MAX_HIDDEN && !npc.hidden.contains(&uuid) {
                            return Err(fail(
                                language,
                                "hidden_limit",
                                &[&id_text, &model::MAX_HIDDEN.to_string()],
                            ));
                        }
                        npc.hidden.insert(uuid.clone());
                        Ok((None, "hidden", vec![id_text.clone(), name.clone()]))
                    } else {
                        npc.hidden.remove(&uuid);
                        Ok((None, "shown", vec![id_text.clone(), name.clone()]))
                    }
                })?;
                count += 1;
            }
            Ok(count)
        }
        Op::ActionAdd => {
            let trigger_text = word(args, "trigger").unwrap_or_default();
            let Some(trigger) = model::parse_trigger(&trigger_text) else {
                return Err(fail(language, "bad_trigger", &[]));
            };
            let here = player.map(place_of);
            let action =
                parse_action_args(args, here.as_ref(), language, console_right(sender, server))?;
            let saved = saved_note(&action);
            let reply = apply(&|npc| {
                if npc.actions.len() >= model::MAX_ACTIONS {
                    return Err(fail(
                        language,
                        "too_many_actions",
                        &[&id_text, &model::MAX_ACTIONS.to_string()],
                    ));
                }
                npc.actions.push((trigger, action.clone()));
                Ok((
                    None,
                    "action_added",
                    vec![id_text.clone(), npc.actions.len().to_string()],
                ))
            })?;
            if let Some(key) = saved {
                say(sender, language, key, &[]);
            }
            Ok(reply)
        }
        Op::ActionRemove => {
            let number = index(args, "index").unwrap_or(0);
            apply(&|npc| {
                let Some(i) = number.checked_sub(1).filter(|i| *i < npc.actions.len()) else {
                    return Err(fail(
                        language,
                        "no_action",
                        &[&id_text, &number.to_string()],
                    ));
                };
                npc.actions.remove(i);
                Ok((
                    None,
                    "action_removed",
                    vec![id_text.clone(), number.to_string()],
                ))
            })
        }
        Op::ActionClear => apply(&|npc| {
            npc.actions.clear();
            Ok((None, "actions_cleared", vec![id_text.clone()]))
        }),
        _ => Ok(0),
    }
}

fn word_arg(name: &str) -> CommandNode {
    CommandNode::argument(name, &ArgumentType::String(StringType::SingleWord))
}

fn greedy_arg(name: &str) -> CommandNode {
    CommandNode::argument(name, &ArgumentType::String(StringType::Greedy))
}

fn number_arg(name: &str) -> CommandNode {
    CommandNode::argument(name, &ArgumentType::Integer((Some(1), Some(64))))
}

fn npc_command() -> Command {
    let id = || word_arg("id");
    Command::new(&["npc".to_string()], "Make and change NPCs")
        .execute(Cmd(Op::Usage))
        .then(CommandNode::literal("list").execute(Cmd(Op::List)))
        .then(CommandNode::literal("reload").execute(Cmd(Op::Reload)))
        .then(CommandNode::literal("stats").execute(Cmd(Op::Stats)))
        .then(CommandNode::literal("import").execute(Cmd(Op::Import)))
        .then(
            CommandNode::literal("create").then(
                id().execute(Cmd(Op::Create))
                    .then(word_arg("type").execute(Cmd(Op::Create))),
            ),
        )
        .then(CommandNode::literal("remove").then(id().execute(Cmd(Op::Remove))))
        .then(CommandNode::literal("info").then(id().execute(Cmd(Op::Info))))
        .then(CommandNode::literal("movehere").then(id().execute(Cmd(Op::MoveHere))))
        .then(
            CommandNode::literal("name").then(
                id().execute(Cmd(Op::NameClear))
                    .then(greedy_arg("text").execute(Cmd(Op::Name))),
            ),
        )
        .then(
            CommandNode::literal("namefor").then(
                id().then(
                    word_arg("language")
                        .execute(Cmd(Op::NameForClear))
                        .then(greedy_arg("text").execute(Cmd(Op::NameFor))),
                ),
            ),
        )
        .then(CommandNode::literal("sit").then(
            id().then(CommandNode::argument("value", &ArgumentType::Bool).execute(Cmd(Op::Sit))),
        ))
        .then(CommandNode::literal("lie").then(
            id().then(CommandNode::argument("value", &ArgumentType::Bool).execute(Cmd(Op::Lie))),
        ))
        .then(
            CommandNode::literal("skin").then(
                id().execute(Cmd(Op::SkinClear))
                    .then(word_arg("account").execute(Cmd(Op::Skin))),
            ),
        )
        .then(
            // A texture has `:` and `/`, which one command word does not take.
            CommandNode::literal("texture").then(
                id().execute(Cmd(Op::TextureClear))
                    .then(greedy_arg("texture").execute(Cmd(Op::Texture))),
            ),
        )
        .then(CommandNode::literal("type").then(id().then(word_arg("type").execute(Cmd(Op::Type)))))
        .then(CommandNode::literal("glow").then(
            id().then(CommandNode::argument("value", &ArgumentType::Bool).execute(Cmd(Op::Glow))),
        ))
        .then(CommandNode::literal("look").then(
            id().then(CommandNode::argument("value", &ArgumentType::Bool).execute(Cmd(Op::Look))),
        ))
        .then(
            CommandNode::literal("scale").then(
                id().then(
                    CommandNode::argument(
                        "size",
                        &ArgumentType::Double((
                            Some(f64::from(model::MIN_SCALE)),
                            Some(f64::from(model::MAX_SCALE)),
                        )),
                    )
                    .execute(Cmd(Op::Scale)),
                ),
            ),
        )
        .then(CommandNode::literal("baby").then(
            id().then(CommandNode::argument("value", &ArgumentType::Bool).execute(Cmd(Op::Baby))),
        ))
        .then(CommandNode::literal("namevisible").then(id().then(
            CommandNode::argument("value", &ArgumentType::Bool).execute(Cmd(Op::NameVisible)),
        )))
        .then(
            CommandNode::literal("range").then(
                id().then(
                    CommandNode::argument("blocks", &ArgumentType::Integer((Some(1), Some(1024))))
                        .execute(Cmd(Op::Range)),
                ),
            ),
        )
        .then(
            CommandNode::literal("cooldown").then(
                id().then(
                    CommandNode::argument("ms", &ArgumentType::Integer((Some(0), Some(60_000))))
                        .execute(Cmd(Op::Cooldown)),
                ),
            ),
        )
        .then(
            CommandNode::literal("permission").then(
                id().execute(Cmd(Op::PermissionClear))
                    .then(word_arg("node").execute(Cmd(Op::Permission))),
            ),
        )
        .then(CommandNode::literal("hide").then(
            id().then(
                CommandNode::argument("players", &ArgumentType::Players).execute(Cmd(Op::Hide)),
            ),
        ))
        .then(CommandNode::literal("show").then(
            id().then(
                CommandNode::argument("players", &ArgumentType::Players).execute(Cmd(Op::Show)),
            ),
        ))
        .then(
            CommandNode::literal("action").then(
                id().then(CommandNode::literal("add").then(
                    word_arg("trigger").then(
                        word_arg("kind").then(greedy_arg("value").execute(Cmd(Op::ActionAdd))),
                    ),
                ))
                .then(
                    CommandNode::literal("remove")
                        .then(number_arg("index").execute(Cmd(Op::ActionRemove))),
                )
                .then(CommandNode::literal("clear").execute(Cmd(Op::ActionClear))),
            ),
        )
        .then(
            CommandNode::literal("menu")
                .then(CommandNode::literal("list").execute(Cmd(Op::MenuList)))
                .then(
                    CommandNode::literal("create").then(
                        word_arg("menu").then(greedy_arg("title").execute(Cmd(Op::MenuCreate))),
                    ),
                )
                .then(
                    CommandNode::literal("remove")
                        .then(word_arg("menu").execute(Cmd(Op::MenuRemove))),
                )
                .then(
                    CommandNode::literal("info").then(word_arg("menu").execute(Cmd(Op::MenuInfo))),
                )
                .then(
                    CommandNode::literal("button").then(
                        word_arg("menu").then(greedy_arg("label").execute(Cmd(Op::MenuButton))),
                    ),
                )
                .then(CommandNode::literal("action").then(word_arg("menu").then(
                    number_arg("index").then(
                        word_arg("kind").then(greedy_arg("value").execute(Cmd(Op::MenuAction))),
                    ),
                ))),
        )
}

// ---------------------------------------------------------------- the plugin message API

/// A plugin message: a JSON object is the API (`api`), the plain words are the older visibility
/// requests: `hide <npc> <uuid>`, `show <npc> <uuid>` (answer `ok`) or `visible <npc> <uuid>`
/// (answer `true` or `false`). A plugin's NPC is named `<plugin>.<key>` there, and only that plugin
/// changes who sees it.
fn ipc(from: &str, payload: &[u8]) -> std::result::Result<Vec<u8>, String> {
    if payload.first() == Some(&b'{') {
        let request = api::parse(payload)?;
        if !model::valid_owner(from) {
            return Err("this plugin's name cannot own NPCs".into());
        }
        let answer = with(|st| api_run(st, from, request))?;
        return Ok(answer.to_string().into_bytes());
    }
    let text = std::str::from_utf8(payload).map_err(|_| "not UTF-8".to_string())?;
    let mut words = text.split_whitespace();
    let (Some(verb), Some(id), Some(uuid), None) =
        (words.next(), words.next(), words.next(), words.next())
    else {
        return Err("expected: hide|show|visible <npc> <uuid>".into());
    };
    let uuid = wire::parse_uuid(uuid)
        .map(wire::uuid_string)
        .ok_or("bad uuid")?;
    with(|st| {
        if st.broken.is_some() && verb != "visible" {
            return Err(FILE_BROKEN.to_string());
        }
        let npc = st
            .store
            .npcs
            .get_mut(id)
            .ok_or_else(|| format!("no NPC {id}"))?;
        if verb != "visible" && npc.owner.as_deref().is_some_and(|owner| owner != from) {
            return Err(format!("NPC {id} belongs to another plugin"));
        }
        let persist = npc.persist;
        let answer = match verb {
            "visible" => {
                return Ok(if npc.hidden.contains(&uuid) {
                    b"false".to_vec()
                } else {
                    b"true".to_vec()
                });
            }
            "hide" => {
                if npc.hidden.len() >= model::MAX_HIDDEN && !npc.hidden.contains(&uuid) {
                    return Err("hidden list full".into());
                }
                npc.hidden.insert(uuid);
                b"ok".to_vec()
            }
            "show" => {
                npc.hidden.remove(&uuid);
                b"ok".to_vec()
            }
            _ => return Err("expected: hide|show|visible <npc> <uuid>".into()),
        };
        if persist {
            ensure_save(st);
        }
        ensure_scan(st, true);
        Ok(answer)
    })
}

/// Refuses a change to something that is written to the file while the file cannot be read.
fn writable(st: &State, persist: bool) -> std::result::Result<(), String> {
    if persist && st.broken.is_some() {
        return Err(FILE_BROKEN.into());
    }
    Ok(())
}

fn api_run(
    st: &mut State,
    from: &str,
    request: Request,
) -> std::result::Result<serde_json::Value, String> {
    use serde_json::json;
    match request {
        Request::Create { key, spec, skin } => api_create(st, from, &key, *spec, skin),
        Request::Move { key, place } => {
            let id = model::owned_id(from, &key);
            let (num, persist) = owned(st, &id, &key)?;
            writable(st, persist)?;
            if let Some(npc) = st.store.npcs.get_mut(&id) {
                npc.place = place;
            }
            st.rot.remove(&num);
            changed(st, num, Refresh::Respawn);
            Ok(json!({"key": key}))
        }
        Request::Name { key, name } => {
            let id = model::owned_id(from, &key);
            let (num, persist) = owned(st, &id, &key)?;
            writable(st, persist)?;
            if let Some(npc) = st.store.npcs.get_mut(&id) {
                npc.name = name.name;
                npc.names = name.names;
            }
            changed(st, num, Refresh::Meta);
            Ok(json!({"key": key}))
        }
        Request::Remove { key } => {
            let id = model::owned_id(from, &key);
            let Ok((num, persist)) = owned(st, &id, &key) else {
                return Ok(json!({"removed": false}));
            };
            writable(st, persist)?;
            st.store.npcs.remove(&id);
            st.removed.insert(num);
            forget_meta(&mut st.meta, num);
            st.rot.remove(&num);
            st.lookups.retain(|l| l.npc != id);
            if persist {
                ensure_save(st);
            }
            ensure_scan(st, true);
            Ok(json!({"removed": true}))
        }
        Request::Get { key } => Ok(st
            .store
            .npcs
            .get(&model::owned_id(from, &key))
            .map_or_else(|| json!({"found": false}), |npc| api::describe(&key, npc))),
        Request::List => Ok(json!({"keys": st.store.owned_keys(from)})),
        Request::IsNpc { entity_id } => Ok(model::num_of(entity_id)
            .and_then(|num| st.store.by_num(num))
            .map_or_else(
                || json!({"npc": false}),
                |npc| json!({"npc": true, "key": npc.key(), "owner": npc.owner}),
            )),
        Request::Click {
            entity_id,
            player,
            left,
        } => Ok(api_click(st, from, entity_id, player, left)),
    }
}

/// The number of a plugin's NPC and whether it is written to the file.
fn owned(st: &State, id: &str, key: &str) -> std::result::Result<(u32, bool), String> {
    st.store
        .npcs
        .get(id)
        .map(|npc| (npc.num, npc.persist))
        .ok_or_else(|| format!("no NPC {key}"))
}

fn api_create(
    st: &mut State,
    from: &str,
    key: &str,
    mut spec: model::Spec,
    skin: SkinAsk,
) -> std::result::Result<serde_json::Value, String> {
    writable(st, spec.persist)?;
    let id = model::owned_id(from, key);
    let account = match skin {
        SkinAsk::None => None,
        SkinAsk::Viewer => {
            spec.viewer_skin = true;
            None
        }
        SkinAsk::Given(skin) => {
            spec.skin = Some(skin);
            None
        }
        SkinAsk::Account(account) => {
            // The same account again (a plugin makes its NPCs at each load): no new lookup.
            let earlier = st
                .store
                .npcs
                .get(&id)
                .and_then(|npc| npc.skin.clone())
                .filter(|skin| skin.from == account);
            match earlier {
                Some(skin) => {
                    spec.skin = Some(skin);
                    None
                }
                None => Some(account),
            }
        }
    };
    let persist = spec.persist;
    let uuid = strawberry_plugin_api::uuid::generate();
    let put = st
        .store
        .put_owned(from, key, spec, key_of(&uuid))
        .map_err(|e| match e {
            CreateError::BadId => "bad key or place".to_string(),
            CreateError::Exists => "exists".to_string(),
            CreateError::Limit => "the NPC limit is reached".to_string(),
        })?;
    st.rot.remove(&put.num);
    st.lookups.retain(|l| l.npc != id);
    if let Some(account) = account {
        st.lookups.push(Lookup {
            npc: id,
            job: lookup(&account),
            account,
            asker: None,
        });
        ensure_lookups(st);
    }
    if put.replaced {
        changed(st, put.num, Refresh::Respawn);
    }
    if persist || put.was_persisted {
        ensure_save(st);
    }
    ensure_scan(st, true);
    Ok(serde_json::json!({
        "key": key,
        "entity_id": model::entity_id(put.num),
        "replaced": put.replaced,
    }))
}

/// A plugin asks whether a click it saw on one of its NPCs counts.
fn api_click(
    st: &mut State,
    from: &str,
    entity_id: i32,
    player: Key,
    left: bool,
) -> serde_json::Value {
    use serde_json::json;
    let refused = json!({"accepted": false});
    let Some(num) = model::num_of(entity_id) else {
        return refused;
    };
    let Some((key, has_actions)) = st
        .store
        .by_num(num)
        .filter(|npc| npc.owner.as_deref() == Some(from))
        .map(|npc| (npc.key().to_string(), !npc.actions.is_empty()))
    else {
        return refused;
    };
    // Its clicks run its actions here; an answer as well would count one click twice.
    if has_actions {
        return json!({"accepted": false, "reason": "npc-lite runs this NPC's actions itself"});
    }
    let who = st
        .server
        .as_ref()
        .and_then(|server| server.get_player_by_uuid(uuid_of(player)))
        .and_then(|player| clicker(&player));
    let Some(who) = who else {
        return refused;
    };
    match check_click(st, num, &who, now_ms()) {
        Some(ClickVerdict::Run) => json!({
            "accepted": true,
            "key": key,
            "click": if left { "left" } else { "right" },
        }),
        Some(ClickVerdict::Wait(left)) => json!({"accepted": false, "wait_ms": left}),
        Some(ClickVerdict::Duplicate) | None => refused,
    }
}

// ---------------------------------------------------------------- the plugin

struct NpcLite;

fn observer() -> HandlerOptions {
    HandlerOptions {
        priority: HandlerPriority::Monitor,
        blocking: false,
        ignore_cancelled: false,
        on_failure: FailureVerdict::Allow,
    }
}

impl Plugin for NpcLite {
    fn new() -> Self {
        Self
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: PLUGIN_NAME.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Strawberry Interactive LLC".into()],
            description: "NPCs that exist only as packets: player skins, mobs, clicks, menus."
                .into(),
            dependencies: vec![],
            permissions: vec![
                permissions::FS_READ_DATA.into(),
                permissions::FS_WRITE_DATA.into(),
            ],
        }
    }

    fn on_load(&self, context: Context) -> Result<()> {
        let loaded = load();
        let server = context.get_server();
        with(|st| {
            *st = State::default();
            match loaded {
                Ok(store) => st.store = store,
                Err(reason) => {
                    log(
                        LogLevel::Error,
                        &format!(
                            "npc-lite: {DATA_FILE} could not be read ({reason}); it is kept as it is and NPCs cannot be changed until it is fixed"
                        ),
                    );
                    st.broken = Some(reason);
                }
            }
            st.server = Some(server);
            if !st.store.npcs.is_empty() {
                ensure_scan(st, false);
            }
        });
        context.register_permission(&Permission {
            node: ADMIN_NODE.into(),
            description: "Make and change NPCs with /npc".into(),
            default: PermissionDefault::Op(PermissionLevel::Two),
            children: vec![],
        })?;
        context.register_permission(&Permission {
            node: CONSOLE_NODE.into(),
            description: "Store an NPC action the console runs, with every right".into(),
            default: PermissionDefault::Op(PermissionLevel::Four),
            children: vec![],
        })?;
        context.register_command(npc_command(), ADMIN_NODE);
        context.register_event_handler_with_options::<PlayerInteractUnknownEntityEvent, _>(
            Click,
            observer(),
        )?;
        context.register_event_handler_with_options::<DialogClickActionEvent, _>(
            MenuClick,
            observer(),
        )?;
        context.register_event_handler_with_options::<PlayerLeaveEvent, _>(Leave, observer())?;
        context.register_event_handler_with_options::<PlayerChangeWorldEvent, _>(
            ChangeWorld,
            observer(),
        )?;
        context
            .register_event_handler_with_options::<PlayerRespawnEvent, _>(Respawn, observer())?;
        context.register_event_handler_with_options::<PlayerLocaleChangeEvent, _>(
            LocaleChange,
            observer(),
        )?;
        // Blocking, so no unload of a script is missed while the server is busy.
        context.register_extension_event_handler(
            PLUGIN_DISABLE,
            ScriptDisabled,
            HandlerOptions {
                blocking: true,
                ..observer()
            },
        )?;
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<()> {
        with(|st| {
            save(st);
            let State {
                server, viewers, ..
            } = st;
            let Some(server) = server.as_ref() else {
                return;
            };
            for (key, viewer) in viewers.iter() {
                if viewer.visible.is_empty() {
                    continue;
                }
                if let Some(java) = server
                    .get_player_by_uuid(uuid_of(*key))
                    .and_then(|p| Sink::of(&p))
                {
                    let mut entity_ids = Vec::with_capacity(viewer.visible.len() * 2);
                    for num in &viewer.visible {
                        push_ids(&mut entity_ids, *num);
                    }
                    send(
                        &java,
                        &ClientboundPacket::CRemoveEntities(CRemoveEntities { entity_ids }),
                    );
                }
            }
            viewers.clear();
        });
        Ok(())
    }

    fn handle_ipc_message(
        &self,
        from: strawberry_plugin_api::wit::PluginId,
        payload: strawberry_plugin_api::wit::IpcMessage,
    ) -> std::result::Result<strawberry_plugin_api::wit::IpcMessage, String> {
        ipc(&from, &payload)
    }
}

register_plugin!(NpcLite);

#[cfg(test)]
mod tests {
    use super::*;

    fn nameless(kind: &str) -> Npc {
        let mut store = model::Store::default();
        let place = Place {
            world: "w".into(),
            x: 0.5,
            y: 64.0,
            z: 0.5,
            yaw: 90.0,
            pitch: 0.0,
        };
        let kind = model::parse_kind(kind).unwrap();
        let mut npc = store
            .create("sleeper", kind, place, (1, 2))
            .unwrap()
            .clone();
        // No name: building the name's text component needs the server.
        npc.name = None;
        npc
    }

    /// Shared flags (no glow), no custom name, its name not shown, silent.
    const STANDING_HEAD: [u8; 12] = [0, 0, 0, 2, 6, 0, 3, 8, 0, 4, 8, 1];
    /// The pose entry: index 6, the pose serializer (20), SLEEPING (2).
    const SLEEPING: [u8; 3] = [6, 20, 2];

    #[test]
    fn a_lying_mob_sends_the_sleeping_pose_and_a_standing_one_sends_what_it_always_did() {
        let mut npc = nameless("minecraft:zombie");
        let standing = build_meta(&npc, "en", None);
        let mut before = STANDING_HEAD.to_vec();
        before.push(0xFF);
        assert_eq!(standing, before, "a standing NPC's bytes do not change");
        npc.lie = true;
        let mut lying = STANDING_HEAD.to_vec();
        lying.extend_from_slice(&SLEEPING);
        lying.push(0xFF);
        assert_eq!(build_meta(&npc, "en", None), lying);
    }

    #[test]
    fn a_lying_mannequin_sends_the_sleeping_pose_beside_its_skin() {
        let mut npc = nameless("player");
        let standing = build_meta(&npc, "en", None);
        assert!(standing.starts_with(&STANDING_HEAD));
        assert!(!standing.windows(3).any(|w| w == SLEEPING));
        npc.lie = true;
        let lying = build_meta(&npc, "en", None);
        let mut expected = STANDING_HEAD.to_vec();
        expected.extend_from_slice(&SLEEPING);
        expected.extend_from_slice(&standing[STANDING_HEAD.len()..]);
        assert_eq!(lying, expected);
    }

    /// A larger NPC is sent its size as the game's scale attribute (so that the client draws it from
    /// farther away); a normal one is sent nothing, as before.
    #[test]
    fn a_scaled_npc_is_sent_the_scale_attribute_and_a_normal_one_nothing() {
        let mut npc = nameless("player");
        assert!(scale_packet(&npc).is_none());
        npc.scale = 3.0;
        let Some(ClientboundPacket::CUpdateAttributes(packet)) = scale_packet(&npc) else {
            panic!("a scaled NPC is sent an update-attributes packet");
        };
        assert_eq!(packet.entity_id, npc.entity_id());
        assert_eq!(packet.properties.len(), 1);
        assert_eq!(packet.properties[0].id, 30, "minecraft:scale");
        assert!((packet.properties[0].value - 3.0).abs() < f64::EPSILON);
        assert!(packet.properties[0].modifiers.is_empty());
    }

    /// Records what a viewer is sent, as the one path does for a Java and a Bedrock viewer.
    struct Recorder {
        bedrock: bool,
        refuse_attributes: bool,
        sent: std::cell::RefCell<Vec<ClientboundPacket>>,
    }

    impl Recorder {
        fn new(bedrock: bool) -> Self {
            Self {
                bedrock,
                refuse_attributes: bedrock,
                sent: std::cell::RefCell::default(),
            }
        }
    }

    impl Wire for Recorder {
        fn send_packet(&self, packet: &ClientboundPacket) -> std::result::Result<(), String> {
            self.sent.borrow_mut().push(packet.clone());
            if self.refuse_attributes && matches!(packet, ClientboundPacket::CUpdateAttributes(_)) {
                return Err("has no Bedrock form here".into());
            }
            Ok(())
        }

        fn is_bedrock(&self) -> bool {
            self.bedrock
        }
    }

    fn names(sent: &[ClientboundPacket]) -> Vec<&'static str> {
        sent.iter()
            .map(|p| match p {
                ClientboundPacket::CSpawnEntity(_) => "spawn",
                ClientboundPacket::CSetEntityMetadata(_) => "data",
                ClientboundPacket::CSetPassengers(_) => "passengers",
                ClientboundPacket::CUpdateAttributes(_) => "attributes",
                ClientboundPacket::CHeadRot(_) => "head",
                ClientboundPacket::CUpdateEntityRot(_) => "turn",
                ClientboundPacket::CRemoveEntities(_) => "remove",
                _ => "other",
            })
            .collect()
    }

    fn dump(sent: &[ClientboundPacket]) -> String {
        format!("{sent:?}")
    }

    fn shown(npc: &Npc, viewer: &Recorder) -> Vec<ClientboundPacket> {
        let mut cache = BTreeMap::new();
        send_npc(viewer, npc, None, "en", &mut cache, &mut OwnSkin::nobody());
        viewer.sent.borrow().clone()
    }

    /// A Bedrock viewer is sent a figure the way a Java viewer is: the same packets in the same
    /// order through the engine call (nothing of the world is made for him). Only the size, an
    /// attribute update the engine has no Bedrock form for, is left out, and nothing is sent for it.
    #[test]
    fn a_bedrock_viewer_is_sent_the_same_figure_packets_as_a_java_viewer() {
        for kind in ["player", "minecraft:zombie"] {
            let mut npc = nameless(kind);
            let java = shown(&npc, &Recorder::new(false));
            let bedrock = shown(&npc, &Recorder::new(true));
            assert_eq!(names(&java), ["spawn", "data"], "{kind}");
            assert_eq!(names(&bedrock), names(&java), "{kind}");
            assert_eq!(
                dump(&bedrock),
                dump(&java),
                "{kind}: the same packets, byte for byte"
            );
            npc.scale = 2.0;
            assert_eq!(
                names(&shown(&npc, &Recorder::new(false))),
                ["spawn", "data", "attributes"]
            );
            let recorder = Recorder::new(true);
            assert_eq!(names(&shown(&npc, &recorder)), ["spawn", "data"]);
            assert!(
                !recorder
                    .sent
                    .borrow()
                    .iter()
                    .any(|p| matches!(p, ClientboundPacket::CUpdateAttributes(_))),
                "an attribute update is never sent to a Bedrock viewer"
            );
        }
    }

    /// A sitting figure reaches a Bedrock viewer with its seat and the ride link, as for Java.
    #[test]
    fn a_sitting_figure_reaches_a_bedrock_viewer_with_its_seat() {
        for kind in ["player", "minecraft:zombie"] {
            let mut npc = nameless(kind);
            npc.sit = true;
            let java = shown(&npc, &Recorder::new(false));
            let bedrock = shown(&npc, &Recorder::new(true));
            assert_eq!(
                names(&bedrock),
                ["spawn", "spawn", "data", "passengers"],
                "{kind}"
            );
            assert_eq!(dump(&bedrock), dump(&java), "{kind}");
            let ClientboundPacket::CSetPassengers(link) = &bedrock[3] else {
                panic!("the ride link comes last");
            };
            assert_eq!(link.entity_id, npc.seat_id());
            assert_eq!(link.passengers, vec![npc.entity_id()]);
        }
    }

    /// Turning to the nearest player sends a Bedrock viewer the head and body turns that changed.
    #[test]
    fn a_figure_turns_to_a_bedrock_viewer_with_the_packets_a_java_viewer_gets() {
        let turn = Turn {
            head: 10,
            body: 20,
            pitch: 30,
        };
        for (head, body, expect) in [
            (true, true, vec!["head", "turn"]),
            (true, false, vec!["head"]),
            (false, true, vec!["turn"]),
            (false, false, vec![]),
        ] {
            let java = Recorder::new(false);
            let bedrock = Recorder::new(true);
            send_turn(&java, 77, turn, head, body);
            send_turn(&bedrock, 77, turn, head, body);
            assert_eq!(names(&bedrock.sent.borrow()), expect);
            assert_eq!(dump(&bedrock.sent.borrow()), dump(&java.sent.borrow()));
        }
    }

    /// A packet the engine refuses does not stop the rest of the figure from being sent.
    #[test]
    fn a_refused_packet_does_not_stop_the_figure_reaching_a_bedrock_viewer() {
        let mut npc = nameless("player");
        npc.scale = 2.0;
        npc.sit = true;
        let viewer = Recorder::new(true);
        viewer.sent.borrow_mut().clear();
        // Refused at the engine: sent anyway when forced through `send`.
        send(&viewer, &scale_packet(&npc).unwrap());
        let rest = shown(&npc, &viewer);
        assert_eq!(names(&rest[1..]), ["spawn", "spawn", "data", "passengers"]);
    }

    fn has(bytes: &[u8], part: &str) -> bool {
        bytes.windows(part.len()).any(|w| w == part.as_bytes())
    }

    #[test]
    fn a_viewer_skin_npc_wears_the_viewers_own_skin_and_no_one_elses() {
        let alex = Skin {
            from: api::VIEWER_SKIN.into(),
            value: "QUxFWA==".into(),
            signature: Some("c2lnLWFsZXg=".into()),
        };
        let steve = Skin {
            from: "Steve".into(),
            value: "U1RFVkU=".into(),
            signature: None,
        };
        let mut npc = nameless("player");
        npc.skin = Some(steve.clone());
        // Without the viewer skin the NPC's own skin is drawn; a viewer's is never used.
        let own = build_meta(&npc, "en", Some(&alex));
        assert!(has(&own, &steve.value) && !has(&own, &alex.value));
        npc.viewer_skin = true;
        let for_alex = build_meta(&npc, "en", Some(&alex));
        assert!(has(&for_alex, &alex.value) && has(&for_alex, "c2lnLWFsZXg="));
        assert!(!has(&for_alex, &steve.value), "only the viewer's own skin");
        // A viewer with no skin sees the default one: no textures at all.
        let plain = build_meta(&npc, "en", None);
        assert!(!has(&plain, &steve.value) && !has(&plain, "textures"));
        // A mob never draws a skin.
        let mut zombie = nameless("minecraft:zombie");
        zombie.viewer_skin = true;
        assert!(!has(&build_meta(&zombie, "en", Some(&alex)), &alex.value));
    }
}
