//! The bytes of the few protocol values an NPC needs that the plugin API takes raw: entity data
//! (`c-set-entity-metadata` carries the serialized entries), the mannequin's profile, angles,
//! UUID text and the entity type ids of Minecraft 26.3.
//!
//! Formats: the public Java Edition protocol documentation for 26.3 (VarInt, String, Boolean,
//! UUID, Prefixed Optional, Prefixed Array, Resolvable Profile, Entity Metadata). Entry indices
//! and serializer ids are the game's data for 26.3.

use crate::types::{BABY_FLAG, ENTITY_TYPES};

/// Entity data index of the shared flags byte (every entity).
pub const FLAGS: u8 = 0;
/// The glowing bit of the shared flags.
pub const FLAG_GLOWING: u8 = 0x40;
/// Entity data index of the custom name (every entity).
pub const CUSTOM_NAME: u8 = 2;
/// Entity data index of "custom name always shown" (every entity).
pub const CUSTOM_NAME_VISIBLE: u8 = 3;
/// Entity data index of "silent" (every entity).
pub const SILENT: u8 = 4;
/// Entity data index of the pose (every entity): `DATA_POSE`, id 6 in every entity of the engine's
/// generated `strawberry-data/src/generated/tracked_data.rs` (read 2026-11-08; not copied).
pub const POSE: u8 = 6;
/// The SLEEPING pose. The pose is sent as its place in the game's pose list: STANDING 0,
/// FALL_FLYING 1, SLEEPING 2, SWIMMING 3, ... (the engine's generated `entity_pose.rs` and the
/// `entity-pose` enum of `strawberry-plugin-wit/v0.1/common.wit` list them in this order). A living
/// entity in it with no bed lies flat along its body's yaw.
pub const POSE_SLEEPING: i32 = 2;
/// The id of the `minecraft:scale` attribute: its place in the `attribute` enum of the plugin API's
/// `attributes` interface, which is the game's attribute registry id (the engine's generated
/// `strawberry-data/src/generated/attributes.rs`, `SCALE`, id 30; read 2026-11-21, not copied).
pub const ATTRIBUTE_SCALE: i32 = 30;
/// Mannequin: which skin layers are shown.
pub const MANNEQUIN_SKIN_PARTS: u8 = 16;
/// Mannequin: the profile its skin comes from.
pub const MANNEQUIN_PROFILE: u8 = 17;
/// Mannequin: cannot be pushed or moved (so it never nudges a player).
pub const MANNEQUIN_IMMOVABLE: u8 = 18;
/// Mannequin: the line under its name.
pub const MANNEQUIN_DESCRIPTION: u8 = 19;
/// The skin models of a profile's skin patch.
pub const MODEL_WIDE: i32 = 0;
pub const MODEL_SLIM: i32 = 1;
/// Every skin layer (cape, jacket, both sleeves, both legs, hat).
pub const ALL_SKIN_PARTS: u8 = 0x7F;

/// The serializer ids of 26.3's entity data.
const SER_BYTE: i32 = 0;
const SER_OPTIONAL_COMPONENT: i32 = 6;
const SER_BOOLEAN: i32 = 8;
/// `MetaDataType::POSE` in the engine's generated `meta_data_type.rs`.
const SER_POSE: i32 = 20;
const SER_RESOLVABLE_PROFILE: i32 = 41;

/// The protocol id of the mannequin, the player-shaped entity that never enters the tab list.
pub const MANNEQUIN: i32 = 84;

/// A growing byte buffer in the protocol's encodings.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct Buf(pub Vec<u8>);

impl Buf {
    /// A VarInt.
    pub fn varint(&mut self, value: i32) {
        let mut rest = value.cast_unsigned();
        loop {
            let low = (rest & 0x7F) as u8;
            rest >>= 7;
            if rest == 0 {
                self.0.push(low);
                return;
            }
            self.0.push(low | 0x80);
        }
    }

    /// A String: its UTF-8 length as a VarInt, then the bytes.
    pub fn string(&mut self, value: &str) {
        self.varint(i32::try_from(value.len()).unwrap_or(i32::MAX));
        self.0.extend_from_slice(value.as_bytes());
    }

    /// A Boolean.
    pub fn boolean(&mut self, value: bool) {
        self.0.push(u8::from(value));
    }

    /// A UUID: the two halves, big-endian.
    pub fn uuid(&mut self, id: (u64, u64)) {
        self.0.extend_from_slice(&id.0.to_be_bytes());
        self.0.extend_from_slice(&id.1.to_be_bytes());
    }
}

/// The skin source of a mannequin: 26.3's partial Resolvable Profile.
#[derive(Debug, Clone, Copy, Default)]
pub struct Profile<'a> {
    pub name: Option<&'a str>,
    pub id: Option<(u64, u64)>,
    /// The `textures` property: its value and signature.
    pub textures: Option<(&'a str, Option<&'a str>)>,
    /// The skin patch: a body texture (`namespace:path` under the pack's `textures`) and the
    /// model (0 wide, 1 slim) the client draws instead of the skin's.
    pub body: Option<&'a str>,
    pub model: Option<i32>,
}

/// Entity data entries, ended with 0xFF by [`Meta::finish`].
#[derive(Default, Debug, Clone)]
pub struct Meta(Buf);

impl Meta {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn head(&mut self, index: u8, serializer: i32) {
        self.0.0.push(index);
        self.0.varint(serializer);
    }

    pub fn byte(&mut self, index: u8, value: u8) {
        self.head(index, SER_BYTE);
        self.0.0.push(value);
    }

    pub fn boolean(&mut self, index: u8, value: bool) {
        self.head(index, SER_BOOLEAN);
        self.0.boolean(value);
    }

    /// A pose, by its place in the game's pose list (see [`POSE_SLEEPING`]).
    pub fn pose(&mut self, index: u8, pose: i32) {
        self.head(index, SER_POSE);
        self.0.varint(pose);
    }

    /// An optional text component, given as its network NBT (what `text-component.encode`
    /// returns).
    pub fn optional_component(&mut self, index: u8, nbt: Option<&[u8]>) {
        self.head(index, SER_OPTIONAL_COMPONENT);
        self.0.boolean(nbt.is_some());
        if let Some(nbt) = nbt {
            self.0.0.extend_from_slice(nbt);
        }
    }

    pub fn profile(&mut self, index: u8, profile: &Profile<'_>) {
        self.head(index, SER_RESOLVABLE_PROFILE);
        let buf = &mut self.0;
        // Kind 0: a partial profile.
        buf.varint(0);
        buf.boolean(profile.name.is_some());
        if let Some(name) = profile.name {
            buf.string(name);
        }
        buf.boolean(profile.id.is_some());
        if let Some(id) = profile.id {
            buf.uuid(id);
        }
        match profile.textures {
            Some((value, signature)) => {
                buf.varint(1);
                buf.string("textures");
                buf.string(value);
                buf.boolean(signature.is_some());
                if let Some(signature) = signature {
                    buf.string(signature);
                }
            }
            None => buf.varint(0),
        }
        // The skin patch: body texture, no cape or elytra override, model.
        buf.boolean(profile.body.is_some());
        if let Some(body) = profile.body {
            buf.string(body);
        }
        buf.boolean(false);
        buf.boolean(false);
        buf.boolean(profile.model.is_some());
        if let Some(model) = profile.model {
            buf.varint(model);
        }
    }

    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        let mut bytes = self.0.0;
        bytes.push(0xFF);
        bytes
    }
}

/// An angle in degrees as the protocol's byte (1/256 of a turn).
#[must_use]
pub fn angle(degrees: f32) -> u8 {
    // `as` saturates and gives 0 for NaN; the low byte is the angle.
    ((degrees * 256.0 / 360.0).floor() as i32 & 0xFF) as u8
}

/// The protocol id and eye height of an entity type, by its name with or without `minecraft:`.
#[must_use]
pub fn entity_type(name: &str) -> Option<(i32, f32)> {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    ENTITY_TYPES
        .iter()
        .find(|t| t.0 == name)
        .map(|t| (t.1, t.2))
}

/// The width and height of an entity type (its bounding box), by name.
#[must_use]
pub fn entity_size(name: &str) -> Option<(f32, f32)> {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    ENTITY_TYPES
        .iter()
        .find(|t| t.0 == name)
        .map(|t| (t.3, t.4))
}

/// The entity data index of the baby flag of a type that has one.
#[must_use]
pub fn baby_index(name: &str) -> Option<u8> {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    BABY_FLAG
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, index)| *index)
}

/// A UUID in its usual text form.
#[must_use]
pub fn uuid_string(id: (u64, u64)) -> String {
    let hex = format!("{:016x}{:016x}", id.0, id.1);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// A UUID from its usual text form (with hyphens, any case); `None` for anything else.
#[must_use]
pub fn parse_uuid(text: &str) -> Option<(u64, u64)> {
    let bytes = text.as_bytes();
    if bytes.len() != 36 {
        return None;
    }
    let mut hex = String::with_capacity(32);
    for (i, b) in bytes.iter().enumerate() {
        if matches!(i, 8 | 13 | 18 | 23) {
            if *b != b'-' {
                return None;
            }
        } else if b.is_ascii_hexdigit() {
            hex.push(char::from(*b));
        } else {
            return None;
        }
    }
    let high = u64::from_str_radix(&hex[..16], 16).ok()?;
    let low = u64::from_str_radix(&hex[16..], 16).ok()?;
    Some((high, low))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(value: i32) -> Vec<u8> {
        let mut buf = Buf::default();
        buf.varint(value);
        buf.0
    }

    #[test]
    fn varints_are_seven_bits_at_a_time_and_negatives_take_five_bytes() {
        assert_eq!(varint(0), [0]);
        assert_eq!(varint(1), [1]);
        assert_eq!(varint(127), [0x7F]);
        assert_eq!(varint(128), [0x80, 0x01]);
        assert_eq!(varint(300), [0xAC, 0x02]);
        assert_eq!(varint(-1), [0xFF, 0xFF, 0xFF, 0xFF, 0x0F]);
        assert_eq!(varint(i32::MAX), [0xFF, 0xFF, 0xFF, 0xFF, 0x07]);
    }

    #[test]
    fn strings_count_bytes_not_characters() {
        let mut buf = Buf::default();
        buf.string("ab");
        assert_eq!(buf.0, [2, b'a', b'b']);
        let mut buf = Buf::default();
        buf.string("ç");
        assert_eq!(buf.0, [2, 0xC3, 0xA7]);
    }

    #[test]
    fn uuids_are_two_big_endian_halves() {
        let mut buf = Buf::default();
        buf.uuid((0x0102_0304_0506_0708, 0x090A_0B0C_0D0E_0F10));
        assert_eq!(buf.0, (1..=16).collect::<Vec<u8>>());
    }

    #[test]
    fn entity_data_entries_end_with_ff() {
        let mut meta = Meta::new();
        meta.byte(FLAGS, FLAG_GLOWING);
        meta.boolean(CUSTOM_NAME_VISIBLE, true);
        meta.optional_component(CUSTOM_NAME, None);
        meta.optional_component(MANNEQUIN_DESCRIPTION, Some(&[10, 0]));
        assert_eq!(
            meta.finish(),
            [0, 0, 0x40, 3, 8, 1, 2, 6, 0, 19, 6, 1, 10, 0, 0xFF]
        );
        assert_eq!(Meta::new().finish(), [0xFF]);
    }

    #[test]
    fn a_pose_is_its_index_the_pose_serializer_and_its_place_in_the_list() {
        let mut meta = Meta::new();
        meta.pose(POSE, POSE_SLEEPING);
        assert_eq!(meta.finish(), [6, 20, 2, 0xFF]);
    }

    #[test]
    fn a_profile_is_partial_with_its_textures_and_no_overrides() {
        let mut meta = Meta::new();
        meta.profile(
            MANNEQUIN_PROFILE,
            &Profile {
                name: None,
                id: Some((1, 2)),
                textures: Some(("v", Some("s"))),
                ..Profile::default()
            },
        );
        let mut want = vec![17, 41, 0, 0, 1];
        want.extend([0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2]);
        want.push(1);
        want.push(8);
        want.extend(b"textures");
        want.extend([1, b'v', 1, 1, b's']);
        want.extend([0, 0, 0, 0, 0xFF]);
        assert_eq!(meta.finish(), want);

        let mut meta = Meta::new();
        meta.profile(
            MANNEQUIN_PROFILE,
            &Profile {
                name: Some("Steve"),
                id: None,
                textures: Some(("v", None)),
                ..Profile::default()
            },
        );
        let mut want = vec![17, 41, 0, 1, 5];
        want.extend(b"Steve");
        want.push(0);
        want.push(1);
        want.push(8);
        want.extend(b"textures");
        want.extend([1, b'v', 0]);
        want.extend([0, 0, 0, 0, 0xFF]);
        assert_eq!(meta.finish(), want);

        let mut meta = Meta::new();
        meta.profile(MANNEQUIN_PROFILE, &Profile::default());
        assert_eq!(meta.finish(), [17, 41, 0, 0, 0, 0, 0, 0, 0, 0, 0xFF]);
    }

    #[test]
    fn a_profile_carries_a_pack_texture_and_its_model_in_the_patch() {
        // Patch: body (Prefixed Optional Identifier), cape, elytra, model (Prefixed Optional
        // VarInt: wide 0, slim 1).
        let mut meta = Meta::new();
        meta.profile(
            MANNEQUIN_PROFILE,
            &Profile {
                id: Some((1, 2)),
                body: Some("br:npc/watcher"),
                model: Some(1),
                ..Profile::default()
            },
        );
        let mut want = vec![17, 41, 0, 0, 1];
        want.extend([0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2]);
        want.push(0);
        want.extend([1, 14]);
        want.extend(b"br:npc/watcher");
        want.extend([0, 0, 1, 1, 0xFF]);
        assert_eq!(meta.finish(), want);

        let mut meta = Meta::new();
        meta.profile(
            MANNEQUIN_PROFILE,
            &Profile {
                body: Some("a"),
                model: Some(0),
                ..Profile::default()
            },
        );
        assert_eq!(
            meta.finish(),
            [17, 41, 0, 0, 0, 0, 1, 1, b'a', 0, 0, 1, 0, 0xFF]
        );
    }

    #[test]
    fn angles_are_packed_like_the_game_packs_them() {
        assert_eq!(angle(0.0), 0);
        assert_eq!(angle(90.0), 64);
        assert_eq!(angle(-90.0), 192);
        assert_eq!(angle(180.0), 128);
        assert_eq!(angle(-180.0), 128);
        assert_eq!(angle(359.9), 255);
        assert_eq!(angle(360.0), 0);
        assert_eq!(angle(1.0), 0);
        assert_eq!(angle(2.0), 1);
        assert_eq!(angle(f32::NAN), 0);
    }

    #[test]
    fn entity_types_are_found_with_or_without_the_namespace() {
        assert_eq!(entity_type("villager"), Some((143, 1.62)));
        assert_eq!(entity_type("minecraft:zombie"), Some((154, 1.74)));
        assert_eq!(entity_type("mannequin"), Some((MANNEQUIN, 1.62)));
        assert_eq!(entity_type("Villager"), None);
        assert_eq!(entity_type("other:villager"), None);
        assert_eq!(entity_type(""), None);
        assert_eq!(ENTITY_TYPES.len(), 161);
        assert_eq!(entity_size("minecraft:allay"), Some((0.35, 0.6)));
        assert_eq!(entity_size("mannequin"), Some((0.6, 1.8)));
        assert_eq!(entity_size("unicorn"), None);
        let mut ids: Vec<i32> = ENTITY_TYPES.iter().map(|t| t.1).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 161, "every protocol id once");
    }

    #[test]
    fn only_types_with_a_baby_flag_have_its_index() {
        assert_eq!(baby_index("villager"), Some(16));
        assert_eq!(baby_index("minecraft:zombie"), Some(16));
        assert_eq!(baby_index("piglin"), Some(17));
        assert_eq!(baby_index("cat"), Some(16));
        assert_eq!(baby_index("allay"), None);
        assert_eq!(baby_index("slime"), None);
        assert_eq!(baby_index("mannequin"), None);
        for (name, _) in BABY_FLAG {
            assert!(entity_type(name).is_some(), "{name} is an entity type");
        }
    }

    #[test]
    fn uuid_text_round_trips_and_rejects_anything_else() {
        let id = (0x0123_4567_89AB_CDEF, 0xFEDC_BA98_7654_3210);
        let text = uuid_string(id);
        assert_eq!(text, "01234567-89ab-cdef-fedc-ba9876543210");
        assert_eq!(parse_uuid(&text), Some(id));
        assert_eq!(parse_uuid("01234567-89AB-CDEF-FEDC-BA9876543210"), Some(id));
        assert_eq!(parse_uuid("0123456789abcdeffedcba9876543210"), None);
        assert_eq!(parse_uuid("01234567-89ab-cdef-fedc-ba987654321"), None);
        assert_eq!(parse_uuid("01234567-89ab-cdef-fedc-ba98765432100"), None);
        assert_eq!(parse_uuid("0123456g-89ab-cdef-fedc-ba9876543210"), None);
        assert_eq!(parse_uuid("+1234567-89ab-cdef-fedc-ba9876543210"), None);
        assert_eq!(parse_uuid("ççç"), None);
    }
}
