//! The API for other plugins, over the plugin messages (`ipc.send-ipc-message`). A request is one
//! JSON object; the answer is one JSON object, or an error text.
//!
//! ```text
//! {"op":"create","key":"boss","spec":{...}}   -> {"key","entity_id","replaced"}
//! {"op":"move","key":"boss","place":{...}}    -> {"key"}
//! {"op":"name","key":"boss","name":...}       -> {"key"}
//! {"op":"remove","key":"boss"}                -> {"removed":true|false}
//! {"op":"get","key":"boss"}                   -> what the NPC is, or {"found":false}
//! {"op":"list"}                               -> {"keys":[...]}
//! {"op":"is-npc","entity_id":N}               -> {"npc":true,"key","owner"} or {"npc":false}
//! {"op":"click","entity_id":N,"player":"<uuid>","click":"right|left"}
//!                                             -> {"accepted":true,"key","click"}
//!                                                or {"accepted":false[,"wait_ms":N]}
//! ```
//!
//! Keys are scoped to the plugin that asks: the sender's name comes from the server, so a plugin
//! cannot reach another plugin's NPCs. A `name` is `null`, a text, or an object with a text per
//! language (`en`, `fr`, `es`, `tr`). A `skin` is `null`, `{"value","signature"}`,
//! `{"account":"Name"}` or `"viewer"` (each player sees the NPC in his own skin); `texture` and `model` are a skin from the server's resource pack; `actions`
//! are message and ui actions npc-lite runs itself. A field this version does not know is refused: a plugin
//! asking for something it would not get finds out.
//!
//! A click on an NPC without actions comes by asking, not by a call back into the owner: the owner
//! listens to `player-interact-unknown-entity` itself and asks; a call from here into a plugin that is
//! calling here could leave both waiting.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::model::{self, Action, Kind, Npc, Place, Skin, SkinModel, SkinPatch, Spec, Trigger};
use crate::wire;

/// The largest request read: a skin's parts and some room.
pub const MAX_REQUEST: usize = 64 << 10;
/// The account name a skin given as textures shows in `/npc info`.
pub const GIVEN_SKIN: &str = "plugin";
/// The skin that is each viewer's own.
pub const VIEWER_SKIN: &str = "viewer";

/// A skin as a plugin asks for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkinAsk {
    None,
    Given(Skin),
    /// Looked up by account name; the NPC has the default skin until the answer comes.
    Account(String),
    /// Each player who sees the NPC sees it in his own skin.
    Viewer,
}

/// A name as a plugin gives it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NameAsk {
    pub name: Option<String>,
    pub names: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Create {
        key: String,
        // Boxed: a spec is several times the size of every other request.
        spec: Box<Spec>,
        skin: SkinAsk,
    },
    Move {
        key: String,
        place: Place,
    },
    Name {
        key: String,
        name: NameAsk,
    },
    Remove {
        key: String,
    },
    Get {
        key: String,
    },
    List,
    IsNpc {
        entity_id: i32,
    },
    Click {
        entity_id: i32,
        player: (u64, u64),
        left: bool,
    },
}

/// Reads a request. Everything in it is untrusted.
pub fn parse(payload: &[u8]) -> Result<Request, String> {
    if payload.len() > MAX_REQUEST {
        return Err(format!("a request is at most {MAX_REQUEST} bytes"));
    }
    let value: Value = serde_json::from_slice(payload).map_err(|e| format!("not JSON: {e}"))?;
    let object = value.as_object().ok_or("a request is a JSON object")?;
    let op = object
        .get("op")
        .and_then(Value::as_str)
        .ok_or("a request has an op")?;
    match op {
        "create" => {
            only(object, &["op", "key", "spec"])?;
            let spec = object.get("spec").ok_or("create needs a spec")?;
            let (spec, skin) = read_spec(spec)?;
            Ok(Request::Create {
                key: read_key(object)?,
                spec: Box::new(spec),
                skin,
            })
        }
        "move" => {
            only(object, &["op", "key", "place"])?;
            let place = object.get("place").ok_or("move needs a place")?;
            Ok(Request::Move {
                key: read_key(object)?,
                place: read_place(place)?,
            })
        }
        "name" => {
            only(object, &["op", "key", "name"])?;
            let name = object.get("name").ok_or("name needs a name")?;
            Ok(Request::Name {
                key: read_key(object)?,
                name: read_name(name)?,
            })
        }
        "remove" => {
            only(object, &["op", "key"])?;
            Ok(Request::Remove {
                key: read_key(object)?,
            })
        }
        "get" => {
            only(object, &["op", "key"])?;
            Ok(Request::Get {
                key: read_key(object)?,
            })
        }
        "list" => {
            only(object, &["op"])?;
            Ok(Request::List)
        }
        "is-npc" => {
            only(object, &["op", "entity_id"])?;
            Ok(Request::IsNpc {
                entity_id: read_entity_id(object)?,
            })
        }
        "click" => {
            only(object, &["op", "entity_id", "player", "click"])?;
            let player = object
                .get("player")
                .and_then(Value::as_str)
                .and_then(wire::parse_uuid)
                .ok_or("click needs a player UUID")?;
            let left = match object.get("click").and_then(Value::as_str) {
                Some("left") => true,
                Some("right") => false,
                _ => return Err("a click is right or left".into()),
            };
            Ok(Request::Click {
                entity_id: read_entity_id(object)?,
                player,
                left,
            })
        }
        other => Err(format!("unknown op {other}")),
    }
}

/// A field this version does not know is refused.
fn only(object: &Map<String, Value>, known: &[&str]) -> Result<(), String> {
    match object.keys().find(|key| !known.contains(&key.as_str())) {
        Some(key) => Err(format!("unknown field {key}")),
        None => Ok(()),
    }
}

fn read_key(object: &Map<String, Value>) -> Result<String, String> {
    object
        .get("key")
        .and_then(Value::as_str)
        .filter(|key| model::valid_id(key))
        .map(str::to_string)
        .ok_or_else(|| "a key has 1 to 32 characters: a-z, 0-9, _ and -".to_string())
}

fn read_entity_id(object: &Map<String, Value>) -> Result<i32, String> {
    object
        .get("entity_id")
        .and_then(Value::as_i64)
        .and_then(|id| i32::try_from(id).ok())
        .ok_or_else(|| "entity_id is not an entity id".to_string())
}

fn read_place(value: &Value) -> Result<Place, String> {
    let object = value.as_object().ok_or("a place is an object")?;
    only(object, &["world", "x", "y", "z", "yaw", "pitch"])?;
    let number = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_f64)
            .ok_or_else(|| format!("a place needs {key}"))
    };
    let place = Place {
        world: object
            .get("world")
            .and_then(Value::as_str)
            .ok_or("a place needs world")?
            .to_string(),
        x: number("x")?,
        y: number("y")?,
        z: number("z")?,
        yaw: number("yaw")? as f32,
        pitch: number("pitch")? as f32,
    };
    model::valid_place(&place)
        .then_some(place)
        .ok_or_else(|| "the place is outside the world".to_string())
}

fn read_name(value: &Value) -> Result<NameAsk, String> {
    let checked = |text: &str| {
        model::valid_text(text, model::MAX_NAME)
            .then(|| text.to_string())
            .ok_or_else(|| "a name is 1 to 256 characters, no control characters".to_string())
    };
    match value {
        Value::Null => Ok(NameAsk::default()),
        Value::String(text) => Ok(NameAsk {
            name: Some(checked(text)?),
            names: BTreeMap::new(),
        }),
        Value::Object(list) => {
            let mut names = BTreeMap::new();
            for (language, text) in list {
                let text = text.as_str().ok_or("a name is text")?;
                names.insert(language.clone(), checked(text)?);
            }
            if !model::valid_names(&names) {
                return Err("a name's language is en, fr, es or tr".into());
            }
            // English is what a viewer of any other language reads.
            let name = names.get("en").cloned();
            Ok(NameAsk { name, names })
        }
        _ => Err("a name is null, a text or an object per language".into()),
    }
}

fn read_skin(value: &Value) -> Result<SkinAsk, String> {
    let object = match value {
        Value::Null => return Ok(SkinAsk::None),
        Value::String(word) if word == VIEWER_SKIN => return Ok(SkinAsk::Viewer),
        Value::Object(object) => object,
        _ => return Err("a skin is null, \"viewer\" or an object".into()),
    };
    if let Some(account) = object.get("account") {
        only(object, &["account"])?;
        return account
            .as_str()
            .filter(|name| model::valid_account(name))
            .map(|name| SkinAsk::Account(name.to_string()))
            .ok_or_else(|| "a skin's account is a Minecraft name".to_string());
    }
    only(object, &["value", "signature"])?;
    let signature = match object.get("signature") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => return Err("a skin's signature is text".into()),
    };
    let skin = Skin {
        from: GIVEN_SKIN.into(),
        value: object
            .get("value")
            .and_then(Value::as_str)
            .ok_or("a skin has an account or a value")?
            .to_string(),
        signature,
    };
    if !model::valid_skin(&skin) {
        return Err("a skin's value and signature are base64 within the game's limits".into());
    }
    Ok(SkinAsk::Given(skin))
}

fn read_spec(value: &Value) -> Result<(Spec, SkinAsk), String> {
    let object = value.as_object().ok_or("a spec is an object")?;
    only(
        object,
        &[
            "place",
            "type",
            "name",
            "name_visible",
            "skin",
            "glow",
            "look",
            "baby",
            "sit",
            "lie",
            "scale",
            "persist",
            "range",
            "cooldown_ms",
            "permission",
            "texture",
            "model",
            "actions",
        ],
    )?;
    let boolean = |key: &str, default: bool| match object.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| format!("{key} is true or false")),
    };
    let whole = |key: &str, default: u32| match object.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| format!("{key} is a whole number")),
    };
    let place = read_place(object.get("place").ok_or("a spec needs a place")?)?;
    let kind = match object.get("type") {
        None => Kind::Player,
        Some(value) => {
            let name = value.as_str().ok_or("type is text")?;
            model::parse_kind(name).ok_or_else(|| format!("unknown entity type {name}"))?
        }
    };
    let name = match object.get("name") {
        None => NameAsk::default(),
        Some(value) => read_name(value)?,
    };
    let baby = boolean("baby", false)?;
    if baby && !model::can_be_baby(&kind) {
        return Err(format!("a {} has no baby form", model::kind_name(&kind)));
    }
    let sit = boolean("sit", false)?;
    let lie = boolean("lie", false)?;
    if sit && lie {
        return Err(model::SIT_AND_LIE.into());
    }
    let scale = match object.get("scale") {
        None => model::DEFAULT_SCALE,
        #[allow(clippy::cast_possible_truncation)]
        Some(value) => value
            .as_f64()
            .map(|scale| scale as f32)
            .filter(|scale| model::valid_scale(*scale))
            .ok_or_else(|| {
                format!(
                    "a scale is a number from {} to {}",
                    model::MIN_SCALE,
                    model::MAX_SCALE
                )
            })?,
    };
    let range = whole("range", model::DEFAULT_RANGE)?;
    if !model::valid_range(range) {
        return Err(format!(
            "a range is {} to {} blocks",
            model::MIN_RANGE,
            model::MAX_RANGE
        ));
    }
    let cooldown_ms = whole("cooldown_ms", model::DEFAULT_COOLDOWN_MS)?;
    if cooldown_ms > model::MAX_COOLDOWN_MS {
        return Err(format!(
            "a cooldown is at most {} ms",
            model::MAX_COOLDOWN_MS
        ));
    }
    let permission = match object.get("permission") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_str()
                .filter(|node| model::valid_node(node))
                .ok_or("a permission has a-z, 0-9, ., _, - and :")?
                .to_string(),
        ),
    };
    let skin = match object.get("skin") {
        None => SkinAsk::None,
        Some(value) => read_skin(value)?,
    };
    let skin_patch = read_patch(object, &kind)?;
    let actions = match object.get("actions") {
        None => Vec::new(),
        Some(value) => read_actions(value)?,
    };
    Ok((
        Spec {
            kind,
            place,
            name: name.name,
            names: name.names,
            name_visible: boolean("name_visible", true)?,
            skin: None,
            viewer_skin: false,
            skin_patch,
            glow: boolean("glow", false)?,
            look: boolean("look", false)?,
            baby,
            sit,
            lie,
            scale,
            persist: boolean("persist", true)?,
            range,
            cooldown_ms,
            permission,
            actions,
        },
        skin,
    ))
}

/// `texture` (`namespace:path` in the server's resource pack) and `model` (`wide` or `slim`, only
/// with a texture), for a player NPC.
fn read_patch(object: &Map<String, Value>, kind: &Kind) -> Result<Option<SkinPatch>, String> {
    let Some(texture) = object.get("texture") else {
        return match object.get("model") {
            None => Ok(None),
            Some(_) => Err("a model comes with a texture".into()),
        };
    };
    if *kind != Kind::Player {
        return Err("only a player NPC has a texture".into());
    }
    let texture = texture
        .as_str()
        .filter(|t| model::valid_texture(t))
        .ok_or("a texture is namespace:path of a-z, 0-9, _, -, . and /")?;
    let model = match object.get("model") {
        None => SkinModel::Wide,
        Some(value) => value
            .as_str()
            .and_then(model::parse_model)
            .ok_or("a model is wide or slim")?,
    };
    Ok(Some(SkinPatch {
        texture: texture.to_string(),
        model,
    }))
}

/// What a click does: `[{"on":"right|left|any","message":...}]` or `{"on":...,"ui":"<file>"}`, a
/// click is a right click unless `on` says otherwise; a message is a text, or an object with a text
/// per language that has `en` (what a player of any other language reads); `ui` opens the server's
/// own `user_interface` file of that id, which that plugin opens only when the server owner gave it
/// a command, key or item. npc-lite runs them on its own click check, so the plugin is not asked
/// per click.
fn read_actions(value: &Value) -> Result<Vec<(Trigger, Action)>, String> {
    let list = value.as_array().ok_or("actions is a list")?;
    if list.len() > model::MAX_ACTIONS {
        return Err(format!("at most {} actions", model::MAX_ACTIONS));
    }
    let checked = |text: &str| {
        model::valid_text(text, model::MAX_MESSAGE)
            .then(|| text.to_string())
            .ok_or_else(|| "a message is 1 to 512 characters, no control characters".to_string())
    };
    let mut actions = Vec::new();
    for entry in list {
        let object = entry.as_object().ok_or("an action is an object")?;
        only(object, &["on", "message", "ui"])?;
        let on = match object.get("on") {
            None => Trigger::Right,
            Some(value) => value
                .as_str()
                .and_then(model::parse_trigger)
                .ok_or("on is right, left or any")?,
        };
        if let Some(file) = object.get("ui") {
            if object.contains_key("message") {
                return Err("an action is a message or a ui, not both".into());
            }
            let file = file
                .as_str()
                .filter(|file| model::valid_ui_file(file))
                .ok_or("a ui is a file id: 1 to 64 of a-z, 0-9, _ . : -")?;
            actions.push((on, Action::Ui(file.to_string())));
            continue;
        }
        let action = match object.get("message") {
            Some(Value::String(text)) => Action::message(&checked(text)?),
            Some(Value::Object(lines)) => {
                let mut texts = BTreeMap::new();
                for (language, line) in lines {
                    let line = line.as_str().ok_or("a message is text")?;
                    texts.insert(language.clone(), checked(line)?);
                }
                if !model::valid_texts(&texts, model::MAX_MESSAGE) {
                    return Err("a message's language is en, fr, es or tr".into());
                }
                let text = texts.remove("en").ok_or("a message per language has en")?;
                Action::Message { text, texts }
            }
            Some(_) => return Err("a message is a text or an object per language".into()),
            None => return Err("an action is a message or a ui".into()),
        };
        actions.push((on, action));
    }
    Ok(actions)
}

/// What an NPC is, as `get` answers: the skin's textures are left out, they carry an account's
/// name and profile id.
#[must_use]
pub fn describe(key: &str, npc: &Npc) -> Value {
    json!({
        "found": true,
        "key": key,
        "owner": npc.owner,
        "entity_id": npc.entity_id(),
        "type": model::kind_name(&npc.kind),
        "name": npc.name,
        "names": npc.names,
        "name_visible": npc.name_visible,
        "skin": if npc.viewer_skin {
            Some(VIEWER_SKIN)
        } else {
            npc.skin.as_ref().map(|skin| skin.from.as_str())
        },
        "place": {
            "world": npc.place.world,
            "x": npc.place.x,
            "y": npc.place.y,
            "z": npc.place.z,
            "yaw": npc.place.yaw,
            "pitch": npc.place.pitch,
        },
        "glow": npc.glow,
        "look": npc.look,
        "baby": npc.baby,
        "sit": npc.sit,
        "lie": npc.lie,
        "scale": npc.scale,
        "persist": npc.persist,
        "range": npc.range,
        "cooldown_ms": npc.cooldown_ms,
        "permission": npc.permission,
        "texture": npc.skin_patch.as_ref().map(|patch| patch.texture.as_str()),
        "model": npc.skin_patch.as_ref().map(|patch| model::model_name(patch.model)),
        "actions": npc.actions.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLACE: &str = r#"{"world":"moon","x":-3.1,"y":64,"z":53.6,"yaw":10.8,"pitch":-0.1}"#;

    fn create(spec_fields: &str) -> Result<Request, String> {
        let text =
            format!(r#"{{"op":"create","key":"boss","spec":{{"place":{PLACE}{spec_fields}}}}}"#);
        parse(text.as_bytes())
    }

    fn created(spec_fields: &str) -> (Spec, SkinAsk) {
        match create(spec_fields).unwrap() {
            Request::Create { key, spec, skin } => {
                assert_eq!(key, "boss");
                (*spec, skin)
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_create_needs_only_a_place_and_reads_the_defaults() {
        let (spec, skin) = created("");
        assert_eq!(spec.kind, Kind::Player);
        assert_eq!(spec.place.world, "moon");
        assert_eq!(spec.name, None);
        assert!(spec.name_visible && spec.persist);
        assert!(!spec.glow && !spec.look && !spec.baby && !spec.sit && !spec.lie);
        assert_eq!(spec.range, model::DEFAULT_RANGE);
        assert!((spec.scale - model::DEFAULT_SCALE).abs() < f32::EPSILON);
        assert_eq!(spec.cooldown_ms, model::DEFAULT_COOLDOWN_MS);
        assert_eq!(skin, SkinAsk::None);
    }

    #[test]
    fn a_create_reads_every_field() {
        let (spec, skin) = created(
            r#","type":"minecraft:piglin","name":{"en":"&2Worker","tr":"&2Çalışan"},"name_visible":false,
               "skin":{"account":"Notch"},"glow":true,"look":true,"baby":true,"sit":true,"persist":false,
               "range":64,"scale":3,"cooldown_ms":1000,"permission":"server.vip""#,
        );
        assert_eq!(spec.kind, Kind::Mob("piglin".into()));
        assert_eq!(
            spec.name.as_deref(),
            Some("&2Worker"),
            "english is the default"
        );
        assert_eq!(spec.names["tr"], "&2Çalışan");
        assert!(!spec.name_visible && spec.glow && spec.look && spec.baby && spec.sit);
        assert!(!spec.persist);
        assert_eq!(spec.range, 64);
        assert!((spec.scale - 3.0).abs() < f32::EPSILON);
        assert_eq!(spec.cooldown_ms, 1000);
        assert_eq!(spec.permission.as_deref(), Some("server.vip"));
        assert_eq!(skin, SkinAsk::Account("Notch".into()));
        let (_, given) = created(r#","skin":{"value":"dGV4dA==","signature":"c2ln"}"#);
        assert_eq!(
            given,
            SkinAsk::Given(Skin {
                from: GIVEN_SKIN.into(),
                value: "dGV4dA==".into(),
                signature: Some("c2ln".into()),
            })
        );
        let (plain, _) = created(r#","name":"&2Boss""#);
        assert_eq!(plain.name.as_deref(), Some("&2Boss"));
        assert!(plain.names.is_empty());
    }

    #[test]
    fn a_create_reads_lie_and_get_answers_it() {
        let (spec, _) = created(r#","type":"minecraft:zombie","lie":true"#);
        assert!(spec.lie && !spec.sit);
        let (spec, _) = created(r#","lie":false,"sit":true"#);
        assert!(!spec.lie && spec.sit);
        assert_eq!(
            create(r#","sit":true,"lie":true"#).unwrap_err(),
            model::SIT_AND_LIE
        );
        let (lying, _) = created(r#","lie":true"#);
        let mut store = model::Store::default();
        store.put_owned("core", "boss", lying, (1, 2)).unwrap();
        let answer = describe("boss", &store.npcs["core.boss"]);
        assert_eq!(answer["lie"], json!(true));
        assert_eq!(answer["sit"], json!(false));
    }

    #[test]
    fn a_viewer_skin_is_asked_with_one_word_and_get_answers_it() {
        let (mut spec, skin) = created(r#","skin":"viewer""#);
        assert_eq!(skin, SkinAsk::Viewer);
        for bad in [
            r#","skin":"Viewer""#,
            r#","skin":"mirror""#,
            r#","skin":{"viewer":true}"#,
            r#","skin":true"#,
        ] {
            assert!(create(bad).is_err(), "{bad}");
        }
        spec.viewer_skin = true;
        let mut store = model::Store::default();
        store.put_owned("lobby", "guide", spec, (1, 2)).unwrap();
        let answer = describe("guide", &store.npcs["lobby.guide"]);
        assert_eq!(answer["skin"], json!("viewer"));
    }

    #[test]
    fn what_cannot_be_given_is_refused_never_dropped() {
        for bad in [
            r#","equipment":[]"#,
            r#","type":"unicorn""#,
            r#","type":"marker""#,
            r#","type":7"#,
            r#","range":500"#,
            r#","range":"far""#,
            r#","scale":0"#,
            r#","scale":0.06"#,
            r#","scale":16.5"#,
            r#","scale":-2"#,
            r#","scale":"big""#,
            r#","cooldown_ms":999999"#,
            r#","baby":true"#,
            r#","glow":"yes""#,
            r#","lie":"yes""#,
            r#","sit":true,"lie":true"#,
            r#","name":"""#,
            r#","name":{"de":"x"}"#,
            r#","name":{"en":""}"#,
            r#","name":7"#,
            r#","permission":"Not A Node""#,
            r#","skin":{"account":"bad name"}"#,
            r#","skin":{"value":"not base64!"}"#,
            r#","skin":{"account":"a","value":"dGV4dA=="}"#,
            r#","skin":{"signature":"c2ln"}"#,
            r#","skin":7"#,
        ] {
            assert!(create(bad).is_err(), "{bad}");
        }
        for bad in [
            r#"{"op":"create","key":"boss","spec":{}}"#,
            r#"{"op":"create","key":"boss"}"#,
            r#"{"op":"create","key":"Bad Key","spec":{"place":{"world":"w","x":0,"y":0,"z":0,"yaw":0,"pitch":0}}}"#,
            r#"{"op":"create","key":"boss","spec":{"place":{"world":"w","x":1e99,"y":0,"z":0,"yaw":0,"pitch":0}}}"#,
            r#"{"op":"create","key":"boss","spec":{"place":{"world":"w","x":0,"y":0,"z":0,"yaw":0,"pitch":100}}}"#,
            r#"{"op":"create","key":"boss","spec":{"place":{"world":"w","x":0,"y":0,"z":0}}}"#,
            r#"{"op":"dance"}"#,
            r#"{"key":"boss"}"#,
            r#"{"op":"remove"}"#,
            r#"{"op":"remove","key":"boss","extra":1}"#,
            r#"{"op":"move","key":"boss"}"#,
            r#"{"op":"name","key":"boss"}"#,
            r#"{"op":"is-npc","entity_id":"x"}"#,
            r#"{"op":"is-npc","entity_id":99999999999}"#,
            r#"{"op":"click","entity_id":1,"player":"not-a-uuid","click":"right"}"#,
            r#"{"op":"click","entity_id":1,"player":"01234567-89ab-cdef-fedc-ba9876543210","click":"middle"}"#,
            r#"[]"#,
            r#"not json"#,
        ] {
            assert!(parse(bad.as_bytes()).is_err(), "{bad}");
        }
        assert!(parse(&[0xFF, 0xFE]).is_err());
        assert!(parse(&vec![b' '; MAX_REQUEST + 1]).is_err());
    }

    #[test]
    fn an_unknown_option_op_or_action_is_refused_with_its_name() {
        // The README's "For other scripts: IPC" says a script finds out what this version lacks.
        assert_eq!(
            create(r#","wings":true"#).unwrap_err(),
            "unknown field wings"
        );
        assert_eq!(parse(br#"{"op":"dance"}"#).unwrap_err(), "unknown op dance");
        assert_eq!(
            parse(br#"{"op":"get","key":"boss","extra":1}"#).unwrap_err(),
            "unknown field extra"
        );
        // A command or console action is the server owner's, set with /npc: never over IPC.
        for kind in ["command", "console"] {
            let spec = format!(r#","actions":[{{"{kind}":"say hi"}}]"#);
            assert_eq!(
                create(&spec).unwrap_err(),
                format!("unknown field {kind}"),
                "{kind}"
            );
        }
    }

    #[test]
    fn a_create_reads_a_pack_texture_and_message_actions() {
        let (spec, _) = created(r#","texture":"br:npc/watcher","model":"slim""#);
        assert_eq!(
            spec.skin_patch,
            Some(SkinPatch {
                texture: "br:npc/watcher".into(),
                model: SkinModel::Slim,
            })
        );
        let (wide, _) = created(r#","texture":"br:npc/watcher""#);
        assert_eq!(wide.skin_patch.map(|p| p.model), Some(SkinModel::Wide));
        let (spec, _) = created(
            r#","actions":[{"on":"left","message":"Hi {player}"},
                           {"message":{"en":"Welcome","tr":"Hoş geldin","fr":"Bienvenue"}}]"#,
        );
        assert_eq!(
            spec.actions,
            vec![
                (Trigger::Left, Action::message("Hi {player}")),
                (
                    Trigger::Right,
                    Action::Message {
                        text: "Welcome".into(),
                        texts: BTreeMap::from([
                            ("fr".to_string(), "Bienvenue".to_string()),
                            ("tr".to_string(), "Hoş geldin".to_string()),
                        ]),
                    }
                ),
            ],
            "a click is a right click unless told; English is the line of every other language"
        );
        let (ui, _) = created(r#","actions":[{"ui":"notebook"},{"on":"left","ui":"br:map"}]"#);
        assert_eq!(
            ui.actions,
            vec![
                (Trigger::Right, Action::Ui("notebook".into())),
                (Trigger::Left, Action::Ui("br:map".into())),
            ]
        );
        let (none, _) = created("");
        assert!(none.actions.is_empty() && none.skin_patch.is_none());
        let many = format!(
            r#","actions":[{}]"#,
            vec![r#"{"message":"x"}"#; model::MAX_ACTIONS + 1].join(",")
        );
        for bad in [
            r#","model":"slim""#,
            r#","type":"villager","texture":"br:npc/watcher""#,
            r#","texture":"BR:npc""#,
            r#","texture":7"#,
            r#","texture":"br:npc/watcher","model":"classic""#,
            r#","actions":{}"#,
            r#","actions":[{"on":"right"}]"#,
            r#","actions":[{"on":"middle","message":"x"}]"#,
            r#","actions":[{"message":"x","extra":1}]"#,
            r#","actions":[{"command":"spawn"}]"#,
            r#","actions":[{"message":""}]"#,
            r#","actions":[{"message":{"tr":"Merhaba"}}]"#,
            r#","actions":[{"message":{"en":"Hi","de":"Hallo"}}]"#,
            r#","actions":[{"message":7}]"#,
            r#","actions":[{"ui":"notebook","message":"x"}]"#,
            r#","actions":[{"ui":"../notebook"}]"#,
            r#","actions":[{"ui":""}]"#,
            r#","actions":[{"ui":7}]"#,
            many.as_str(),
        ] {
            assert!(create(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_other_requests_are_read() {
        assert_eq!(parse(br#"{"op":"list"}"#), Ok(Request::List));
        assert_eq!(
            parse(br#"{"op":"remove","key":"boss"}"#),
            Ok(Request::Remove { key: "boss".into() })
        );
        assert_eq!(
            parse(br#"{"op":"get","key":"boss"}"#),
            Ok(Request::Get { key: "boss".into() })
        );
        assert_eq!(
            parse(br#"{"op":"is-npc","entity_id":2046820353}"#),
            Ok(Request::IsNpc {
                entity_id: 2046820353
            })
        );
        assert_eq!(
            parse(
                br#"{"op":"click","entity_id":5,"player":"01234567-89ab-cdef-fedc-ba9876543210","click":"left"}"#
            ),
            Ok(Request::Click {
                entity_id: 5,
                player: (0x0123_4567_89AB_CDEF, 0xFEDC_BA98_7654_3210),
                left: true,
            })
        );
        let text = format!(r#"{{"op":"move","key":"boss","place":{PLACE}}}"#);
        match parse(text.as_bytes()).unwrap() {
            Request::Move { key, place } => {
                assert_eq!(key, "boss");
                assert_eq!(place.world, "moon");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            parse(br#"{"op":"name","key":"boss","name":null}"#),
            Ok(Request::Name {
                key: "boss".into(),
                name: NameAsk::default(),
            })
        );
        assert!(matches!(
            parse(br#"{"op":"name","key":"boss","name":{"tr":"Patron"}}"#),
            Ok(Request::Name { name, .. }) if name.names["tr"] == "Patron" && name.name.is_none()
        ));
    }

    #[test]
    fn get_tells_what_the_npc_is_and_never_the_textures() {
        let mut store = model::Store::default();
        let (mut spec, _) = created(r#","name":"&2Boss","range":112"#);
        spec.skin = Some(Skin {
            from: "Notch".into(),
            value: "SECRETVALUE".into(),
            signature: Some("SECRETSIG".into()),
        });
        store.put_owned("core", "boss", spec, (1, 2)).unwrap();
        let value = describe("boss", &store.npcs["core.boss"]);
        assert_eq!(value["found"], true);
        assert_eq!(value["key"], "boss");
        assert_eq!(value["owner"], "core");
        assert_eq!(value["entity_id"], model::entity_id(1));
        assert_eq!(value["type"], "player");
        assert_eq!(value["name"], "&2Boss");
        assert_eq!(value["range"], 112);
        assert_eq!(value["persist"], true);
        assert_eq!(value["skin"], "Notch");
        assert_eq!(value["place"]["world"], "moon");
        assert_eq!(value["texture"], serde_json::Value::Null);
        assert_eq!(value["actions"], 0);
        let text = value.to_string();
        assert!(!text.contains("SECRET"), "{text}");
        let (patched, _) =
            created(r#","texture":"br:npc/watcher","model":"slim","actions":[{"message":"Hi"}]"#);
        store.put_owned("core", "watcher", patched, (3, 4)).unwrap();
        let value = describe("watcher", &store.npcs["core.watcher"]);
        assert_eq!(value["texture"], "br:npc/watcher");
        assert_eq!(value["model"], "slim");
        assert_eq!(value["actions"], 1);
    }
}
