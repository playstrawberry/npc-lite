//! `/npc import`: reads the YAML NPC saves other server software writes (`data/import.yml`) and
//! says what became of every row. A one-shot: it never changes an NPC that already exists.
//!
//! Only the small part of YAML such a file uses is read. A line it cannot read refuses the whole
//! file: nothing is half imported. A row it cannot place is listed, never guessed. A skin's value is
//! never printed.

use crate::model::{self, Action, Kind, Place, Skin, Trigger};

/// The largest file read (1 MiB): a save of many hundreds of NPCs.
pub const MAX_IMPORT: usize = 1 << 20;
const MAX_DEPTH: usize = 32;
/// The name a skin shows in `/npc info` when its account name cannot be read.
const IMPORTED_SKIN: &str = "imported";
/// Parts of a row that hold nothing an NPC here needs: never reported.
const NOTHING_LOST: [&str; 15] = [
    "scoreboardtrait",
    "owner",
    "spawned",
    "location",
    "type",
    "leashedtrait",
    "chunktickettrait",
    "gravity",
    "inventory",
    "age",
    "skintrait",
    "sittrait",
    "commandtrait",
    "lookclose",
    "equipment",
];

/// A value of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Text(String),
    Map(Vec<(String, Node)>),
    List(Vec<Node>),
}

/// Why a row was not imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skipped {
    /// The row names its world only by an id, which cannot be matched to a world.
    WorldIdOnly,
    /// It was not shown on the server the file comes from.
    NotShown,
    UnknownType(String),
    BadPlace,
}

/// What a row had that the NPC does not get.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dropped {
    /// Items cannot be read from the file (and NPCs have no equipment yet).
    Equipment,
    /// It walked with the game's own AI; here it stands still.
    VanillaAi,
    /// A click command for a kind of click that is not right, left or both.
    ClickType,
    /// A command that is too long, empty or run with extra rights.
    Command,
    /// A skin given only by an account name: set it with `/npc skin`.
    SkinByName,
    /// A part of the row this plugin has no use for.
    Part(String),
}

/// One row that becomes an NPC.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// The name the NPC gets: `npc-<number in the file>`.
    pub id: String,
    pub kind: Kind,
    pub place: Place,
    pub name: Option<String>,
    pub name_visible: bool,
    pub skin: Option<Skin>,
    pub glow: bool,
    pub look: bool,
    pub baby: bool,
    pub sit: bool,
    pub actions: Vec<(Trigger, Action)>,
    pub dropped: Vec<Dropped>,
}

/// What a file holds.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Report {
    pub rows: Vec<Row>,
    /// The number of the row in the file, and why.
    pub skipped: Vec<(String, Skipped)>,
}

/// Takes the `console` actions out of the rows, for a sender without the console right
/// (`dispatch::may_store`): an import never gives an NPC a command with every right that its
/// sender could not have stored by hand. Each row that lost one says so (`Dropped::Command`).
pub fn drop_console_actions(rows: &mut [Row]) {
    for row in rows {
        let before = row.actions.len();
        row.actions
            .retain(|(_, action)| !matches!(action, Action::Console(_)));
        if row.actions.len() < before && !row.dropped.contains(&Dropped::Command) {
            row.dropped.push(Dropped::Command);
        }
    }
}

/// Reads the text of a file. An error names the line, never its content.
pub fn parse_yaml(text: &str) -> Result<Node, String> {
    let raw = text
        .lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let body = line.trim_end();
            let trimmed = body.trim_start();
            (!trimmed.is_empty() && !trimmed.starts_with('#')).then_some(Raw {
                number: i + 1,
                indent: body.len() - trimmed.len(),
                text: trimmed,
            })
        })
        .collect::<Vec<_>>();
    let mut reader = Reader { raw, at: 0 };
    if reader.raw.is_empty() {
        return Ok(Node::Map(Vec::new()));
    }
    let indent = reader.raw[0].indent;
    let node = reader.map(indent, None, 0)?;
    match reader.raw.get(reader.at) {
        Some(line) => Err(format!("line {} cannot be read", line.number)),
        None => Ok(node),
    }
}

struct Raw<'a> {
    number: usize,
    indent: usize,
    text: &'a str,
}

struct Reader<'a> {
    raw: Vec<Raw<'a>>,
    at: usize,
}

impl Reader<'_> {
    fn next_indent(&self) -> Option<usize> {
        self.raw.get(self.at).map(|line| line.indent)
    }

    /// A map whose entries stand at `indent`. `first` is the rest of a `- ` line that opens it.
    fn map(
        &mut self,
        indent: usize,
        first: Option<(usize, String)>,
        depth: usize,
    ) -> Result<Node, String> {
        if depth > MAX_DEPTH {
            return Err("the file is nested too deeply".into());
        }
        let mut entries = Vec::new();
        let mut pending = first;
        loop {
            let (number, text) = match pending.take() {
                Some(line) => line,
                None => {
                    let Some(line) = self.raw.get(self.at) else {
                        break;
                    };
                    if line.indent != indent || line.text.starts_with("- ") || line.text == "-" {
                        break;
                    }
                    self.at += 1;
                    (line.number, line.text.to_string())
                }
            };
            let Some((key, value)) = split_key(&text) else {
                return Err(format!("line {number} cannot be read"));
            };
            let node = self.value(indent, number, value, depth)?;
            entries.push((unquote(key, number)?, node));
        }
        Ok(Node::Map(entries))
    }

    /// The value of a key at `indent`, with what follows it.
    fn value(
        &mut self,
        indent: usize,
        number: usize,
        value: &str,
        depth: usize,
    ) -> Result<Node, String> {
        let value = strip_tag(value);
        if value.is_empty() {
            return match self.next_indent() {
                Some(next) if next > indent => {
                    let dash = self.raw[self.at].text.starts_with("- ");
                    if dash {
                        self.list(next, depth + 1)
                    } else {
                        self.map(next, None, depth + 1)
                    }
                }
                Some(next) if next == indent && self.raw[self.at].text.starts_with("- ") => {
                    self.list(next, depth + 1)
                }
                _ => Ok(Node::Text(String::new())),
            };
        }
        if value.starts_with('|') || value.starts_with('>') {
            let mut lines = Vec::new();
            while self.next_indent().is_some_and(|next| next > indent) {
                lines.push(self.raw[self.at].text);
                self.at += 1;
            }
            return Ok(Node::Text(lines.join("\n")));
        }
        // A value goes on over the deeper lines that follow: quoted, flow or plain.
        let mut joined = value.to_string();
        while self.next_indent().is_some_and(|next| next > indent) {
            joined.push(' ');
            joined.push_str(self.raw[self.at].text);
            self.at += 1;
        }
        if joined.starts_with('{') || joined.starts_with('[') {
            let closed = joined.ends_with('}') || joined.ends_with(']');
            if !closed {
                return Err(format!("line {number} cannot be read"));
            }
            return Ok(Node::Text(joined.replace(' ', "")));
        }
        unquote(&joined, number).map(Node::Text)
    }

    fn list(&mut self, indent: usize, depth: usize) -> Result<Node, String> {
        if depth > MAX_DEPTH {
            return Err("the file is nested too deeply".into());
        }
        let mut items = Vec::new();
        while let Some(line) = self.raw.get(self.at) {
            if line.indent != indent || !line.text.starts_with("- ") {
                break;
            }
            let (number, rest) = (line.number, line.text[2..].trim_start().to_string());
            self.at += 1;
            if split_key(&rest).is_some() {
                // `- a: 1`: a map whose next keys stand two further in.
                items.push(self.map(indent + 2, Some((number, rest)), depth + 1)?);
            } else {
                items.push(Node::Text(unquote(&rest, number)?));
            }
        }
        Ok(Node::List(items))
    }
}

/// `key: value` or `key:`, the key possibly quoted.
fn split_key(text: &str) -> Option<(&str, &str)> {
    let mut quote = None;
    let bytes = text.as_bytes();
    if matches!(bytes.first(), Some(b'{' | b'[')) {
        return None;
    }
    for (i, b) in bytes.iter().enumerate() {
        match (quote, *b) {
            (None, b'\'' | b'"') if i == 0 => quote = Some(*b),
            (Some(q), c) if c == q => quote = None,
            (None, b':') if bytes.get(i + 1).is_none_or(|next| *next == b' ') => {
                return Some((text[..i].trim_end(), text[i + 1..].trim_start()));
            }
            _ => {}
        }
    }
    None
}

/// A tag such as `!!binary` says nothing this reader needs.
fn strip_tag(value: &str) -> &str {
    if value.starts_with("!!") {
        return value
            .split_once(' ')
            .map_or("", |(_, rest)| rest.trim_start());
    }
    value
}

/// A scalar: `'...'` with `''` inside, `"..."` with backslash escapes, or plain.
fn unquote(text: &str, number: usize) -> Result<String, String> {
    let unreadable = || format!("line {number} cannot be read");
    if let Some(rest) = text.strip_prefix('\'') {
        let inner = rest.strip_suffix('\'').ok_or_else(unreadable)?;
        if inner.replace("''", "").contains('\'') {
            return Err(unreadable());
        }
        return Ok(inner.replace("''", "'"));
    }
    if let Some(rest) = text.strip_prefix('"') {
        let inner = rest.strip_suffix('"').ok_or_else(unreadable)?;
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => return Err(unreadable()),
            }
        }
        return Ok(out);
    }
    Ok(text.to_string())
}

impl Node {
    fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn at(&self, path: &[&str]) -> Option<&Node> {
        path.iter().try_fold(self, |node, key| node.get(key))
    }

    fn text(&self) -> Option<&str> {
        match self {
            Node::Text(text) => Some(text),
            _ => None,
        }
    }

    /// Whether this is a value with anything in it.
    fn filled(&self) -> bool {
        match self {
            Node::Text(text) => !text.is_empty() && text != "{}" && text != "[]",
            Node::Map(entries) => !entries.is_empty(),
            Node::List(items) => !items.is_empty(),
        }
    }
}

/// Reads a save file.
pub fn read_saves(text: &str) -> Result<Report, String> {
    if text.len() > MAX_IMPORT {
        return Err(format!("the file is larger than {MAX_IMPORT} bytes"));
    }
    let file = parse_yaml(text)?;
    let Some(Node::Map(rows)) = file.get("npc") else {
        return Err("the file has no npc list".into());
    };
    let mut report = Report::default();
    for (number, row) in rows {
        match read_row(number, row) {
            Ok(row) => report.rows.push(row),
            Err(why) => report.skipped.push((number.clone(), why)),
        }
    }
    Ok(report)
}

fn number_at(node: &Node, key: &str) -> Option<f64> {
    node.get(key)?
        .text()?
        .parse()
        .ok()
        .filter(|v: &f64| v.is_finite())
}

/// A place from a `location` or `sittingAt` part. The world must be there by name.
fn read_place(node: &Node) -> Result<Place, Skipped> {
    let Some(world) = node.get("world").and_then(Node::text) else {
        return Err(Skipped::WorldIdOnly);
    };
    let read = || {
        Some(Place {
            world: world.to_string(),
            x: number_at(node, "x")?,
            y: number_at(node, "y")?,
            z: number_at(node, "z")?,
            yaw: number_at(node, "yaw").unwrap_or(0.0) as f32,
            pitch: number_at(node, "pitch").unwrap_or(0.0) as f32,
        })
    };
    read().filter(model::valid_place).ok_or(Skipped::BadPlace)
}

fn is_true(node: Option<&Node>) -> bool {
    node.and_then(Node::text) == Some("true")
}

fn is_false(node: Option<&Node>) -> bool {
    node.and_then(Node::text) == Some("false")
}

fn read_row(number: &str, row: &Node) -> Result<Row, Skipped> {
    let traits = row.get("traits");
    let from_traits = |path: &[&str]| traits.and_then(|t| t.at(path));
    if is_false(row.at(&["metadata", "spawned"])) || is_false(from_traits(&["spawned"])) {
        return Err(Skipped::NotShown);
    }
    let location = from_traits(&["location"]).ok_or(Skipped::BadPlace)?;
    let seat = from_traits(&["sittrait", "sittingAt"]);
    let place = match seat {
        Some(seat) => read_place(seat)?,
        None => read_place(location)?,
    };
    let type_name = from_traits(&["type"])
        .and_then(Node::text)
        .unwrap_or("PLAYER")
        .to_ascii_lowercase();
    let kind = model::parse_kind(&type_name).ok_or(Skipped::UnknownType(type_name))?;
    let id = format!("npc-{number}");
    if !model::valid_id(&id) {
        return Err(Skipped::BadPlace);
    }
    let mut dropped = Vec::new();
    let name = row
        .get("name")
        .and_then(Node::text)
        .map(|name| name.replace('§', "&"))
        .filter(|name| model::valid_text(name, model::MAX_NAME));
    let name_visible = row
        .at(&["metadata", "nameplate-visible"])
        .and_then(Node::text)
        .is_none_or(|value| value == "true");
    let skin_part = from_traits(&["skintrait"]);
    let skin = skin_part.and_then(read_skin);
    if skin.is_none()
        && skin_part.is_some_and(|part| part.get("skinName").is_some_and(Node::filled))
    {
        dropped.push(Dropped::SkinByName);
    }
    if from_traits(&["equipment"]).is_some_and(Node::filled) {
        dropped.push(Dropped::Equipment);
    }
    if is_true(row.at(&["metadata", "minecraft-ai"])) {
        dropped.push(Dropped::VanillaAi);
    }
    let baby = from_traits(&["age", "age"])
        .and_then(Node::text)
        .and_then(|age| age.parse::<i64>().ok())
        .is_some_and(|age| age < 0)
        && model::can_be_baby(&kind);
    let actions = read_actions(from_traits(&["commandtrait", "commands"]), &mut dropped);
    if let Some(Node::Map(parts)) = traits {
        for (part, _) in parts {
            if !NOTHING_LOST.contains(&part.as_str()) {
                dropped.push(Dropped::Part(part.clone()));
            }
        }
    }
    Ok(Row {
        id,
        kind,
        place,
        name,
        name_visible,
        skin,
        glow: is_true(row.at(&["metadata", "glowing"])),
        look: is_true(from_traits(&["lookclose", "enabled"])),
        baby,
        sit: seat.is_some(),
        actions,
        dropped,
    })
}

/// The stored textures of a skin part; `None` when it holds none that the client would read.
fn read_skin(part: &Node) -> Option<Skin> {
    let value = part.get("textureRaw")?.text()?;
    let from = part
        .get("skinName")
        .and_then(Node::text)
        .filter(|name| model::valid_account(name))
        .unwrap_or(IMPORTED_SKIN);
    let skin = Skin {
        from: from.to_string(),
        value: value.to_string(),
        signature: part
            .get("signature")
            .and_then(Node::text)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    };
    model::valid_skin(&skin).then_some(skin)
}

fn read_actions(commands: Option<&Node>, dropped: &mut Vec<Dropped>) -> Vec<(Trigger, Action)> {
    let Some(Node::List(commands)) = commands else {
        return Vec::new();
    };
    let mut actions = Vec::new();
    for entry in commands {
        let trigger = match entry.get("hand").and_then(Node::text) {
            None | Some("BOTH") => Some(Trigger::Any),
            Some("RIGHT") => Some(Trigger::Right),
            Some("LEFT") => Some(Trigger::Left),
            Some(_) => None,
        };
        let Some(trigger) = trigger else {
            dropped.push(Dropped::ClickType);
            continue;
        };
        let as_player = is_true(entry.get("player"));
        let action = entry
            .get("command")
            .and_then(Node::text)
            .filter(|_| !is_true(entry.get("op")) && actions.len() < model::MAX_ACTIONS)
            .and_then(|text| {
                let kind = if as_player { "command" } else { "console" };
                model::parse_action(kind, text, None).ok()
            });
        match action {
            Some(action) => actions.push((trigger, action)),
            None => dropped.push(Dropped::Command),
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# NPC Storage
last-created-npc-id: 342
npc:
  '8':
    metadata:
      cached-skin-uuid: 05cf000b-6185-44c7-b650-03449aadc9fa
    traits:
      scoreboardtrait:
        tags:
        - SOME_TAG
      owner:
        uuid: 8e202233-cbd6-3b66-b0dc-c358a6bed8e9
      skintrait:
        signature: c2lnbmF0dXJl
        skinName: Some_Name
        textureRaw: dGV4dHVyZQ==
      location:
        world: spawn_world
        x: -72.9438
        y: 63.9
        z: -338.5514
        bodyYaw: 0.13
        pitch: 1.3501
        yaw: 0.1345
      type: PLAYER
      sittrait:
        sittingAt:
          world: spawn_world
          x: -72.9438
          y: 64.5
          z: -338.5514
          pitch: 1.3501
          yaw: 0.1345
    traitnames: scoreboardtrait,spawned,owner
    name: '&aSitter'
    navigator:
      avoidwater: false
    uuid: 7a3c4490-f32d-44c0-a649-f4bf4d2f4e10
  '139':
    metadata:
      minecraft-ai: true
    traits:
      equipment:
        hand:
          amount: 2
          editable_components:
            lore: 'first line
              second line ''quoted''
              last line'
            edited: false
            display_name: §6§lGem
          type: paper
          meta: rO0ABXNyABpvcmc
      location:
        world: spawn_world
        worldid: 6a2f235f-146b-46a6-aa6c-2dad26cd5978
        x: -36.5
        y: 63.0
        z: 114.1
        pitch: 0.0
        yaw: 356.2
      type: PIGLIN
      inventory: {
        }
      age:
        age: -24000
    name: §6Worker
  '311':
    metadata:
      flyable: true
      nameplate-visible: 'false'
    traits:
      commandtrait:
        commands:
        - command: menu open
          hand: RIGHT
        - command: warp spawn
          hand: BOTH
          player: true
        - command: secret
          hand: SHIFT_RIGHT
        - command: give
          hand: LEFT
          op: true
      location:
        world: spawn_world
        x: 1
        y: 66
        z: 2
        pitch: 0
        yaw: 0
      type: ALLAY
      lookclose:
        enabled: true
      someothertrait:
        a: b
    name: /menu
  '10':
    traits:
      location:
        worldid: 8822e911-b550-4dc4-a3c3-302eb7a1cd03
        x: 1
        y: 2
        z: 3
        pitch: 0
        yaw: 0
      type: HORSE
    name: only an id
  '9':
    metadata:
      spawned: false
    traits:
      location:
        world: spawn_world
        x: 1
        y: 2
        z: 3
        pitch: 0
        yaw: 0
      type: PLAYER
    name: was off
  '14':
    traits:
      skintrait:
        skinName: Named
      location:
        world: spawn_world
        x: 1
        y: 2
        z: 3
        pitch: 0
        yaw: 0
      type: PLAYER
    name: named skin
  '20':
    traits:
      location:
        world: spawn_world
        x: 1
        y: 2
        z: 3
        pitch: 0
        yaw: 0
      type: UNICORN
    name: no such mob
  '21':
    traits:
      location:
        world: spawn_world
        x: 1
        y: 99999
        z: 3
        pitch: 0
        yaw: 0
      type: COW
    name: too high
";

    fn row<'a>(report: &'a Report, id: &str) -> &'a Row {
        report.rows.iter().find(|r| r.id == id).unwrap()
    }

    #[test]
    fn the_yaml_reader_reads_what_such_files_hold() {
        let node = parse_yaml(SAMPLE).unwrap();
        let Node::Map(top) = &node else { panic!() };
        assert_eq!(
            top[0],
            ("last-created-npc-id".into(), Node::Text("342".into()))
        );
        assert_eq!(top[1].0, "npc");
        assert_eq!(
            parse_yaml("a: 'x ''y'' z'\nb: \"q\\\"r\"\nc: {}\nd:\n- 1\n- e: 2\n  f: 3\n"),
            Ok(Node::Map(vec![
                ("a".into(), Node::Text("x 'y' z".into())),
                ("b".into(), Node::Text("q\"r".into())),
                ("c".into(), Node::Text("{}".into())),
                (
                    "d".into(),
                    Node::List(vec![
                        Node::Text("1".into()),
                        Node::Map(vec![
                            ("e".into(), Node::Text("2".into())),
                            ("f".into(), Node::Text("3".into())),
                        ]),
                    ])
                ),
            ]))
        );
        assert_eq!(
            parse_yaml("a: plain\n  more\nb: |\n  block\n  text\nc: 1\n"),
            Ok(Node::Map(vec![
                ("a".into(), Node::Text("plain more".into())),
                ("b".into(), Node::Text("block\ntext".into())),
                ("c".into(), Node::Text("1".into())),
            ]))
        );
        assert_eq!(parse_yaml(""), Ok(Node::Map(Vec::new())));
    }

    #[test]
    fn a_line_it_cannot_read_refuses_the_file_and_is_not_printed() {
        let error = parse_yaml("a: 1\nSECRET no colon here\n").unwrap_err();
        assert!(error.contains('2') && !error.contains("SECRET"), "{error}");
        assert!(parse_yaml("a: 'never closed\nb: 2\n").is_err());
        assert!(parse_yaml("a: {never closed\nb: 2\n").is_err());
        let deep: String = (0..64).map(|i| format!("{}k:\n", " ".repeat(i))).collect();
        assert!(parse_yaml(&deep).is_err(), "bounded depth");
        assert!(read_saves(&"x".repeat(MAX_IMPORT + 1)).is_err());
        assert!(read_saves("a: 1\n").is_err(), "a file with no npc list");
    }

    #[test]
    fn a_seated_player_with_a_stored_skin() {
        let report = read_saves(SAMPLE).unwrap();
        let sitter = row(&report, "npc-8");
        assert_eq!(sitter.kind, Kind::Player);
        assert!(sitter.sit);
        assert_eq!(sitter.place.y, 64.5, "the seat's place");
        assert_eq!(sitter.place.world, "spawn_world");
        assert_eq!(sitter.name.as_deref(), Some("&aSitter"));
        assert!(sitter.name_visible);
        let skin = sitter.skin.as_ref().unwrap();
        assert_eq!(skin.from, "Some_Name");
        assert_eq!(skin.value, "dGV4dHVyZQ==");
        assert_eq!(skin.signature.as_deref(), Some("c2lnbmF0dXJl"));
        assert!(sitter.dropped.is_empty(), "{:?}", sitter.dropped);
    }

    #[test]
    fn a_baby_with_items_and_ai_says_what_it_loses() {
        let report = read_saves(SAMPLE).unwrap();
        let worker = row(&report, "npc-139");
        assert_eq!(worker.kind, Kind::Mob("piglin".into()));
        assert!(worker.baby);
        assert_eq!(worker.name.as_deref(), Some("&6Worker"), "§ reads as &");
        assert_eq!(worker.place.x, -36.5);
        assert!(worker.dropped.contains(&Dropped::Equipment));
        assert!(worker.dropped.contains(&Dropped::VanillaAi));
    }

    #[test]
    fn an_import_without_the_console_right_keeps_no_console_action_and_says_so() {
        let mut report = read_saves(SAMPLE).unwrap();
        for row in &mut report.rows {
            row.dropped.retain(|d| *d != Dropped::Command);
        }
        drop_console_actions(&mut report.rows);
        let menu = row(&report, "npc-311");
        assert_eq!(
            menu.actions,
            vec![(Trigger::Any, Action::Command("warp spawn".into()))]
        );
        assert!(menu.dropped.contains(&Dropped::Command));
        assert!(report.rows.iter().all(|r| {
            r.actions
                .iter()
                .all(|(_, a)| !matches!(a, Action::Console(_)))
        }));
    }

    #[test]
    fn click_commands_keep_their_click_and_who_runs_them() {
        let report = read_saves(SAMPLE).unwrap();
        let menu = row(&report, "npc-311");
        assert!(!menu.name_visible);
        assert!(menu.look);
        assert_eq!(
            menu.actions,
            vec![
                (Trigger::Right, Action::Console("menu open".into())),
                (Trigger::Any, Action::Command("warp spawn".into())),
            ]
        );
        assert!(menu.dropped.contains(&Dropped::ClickType));
        assert!(
            menu.dropped.contains(&Dropped::Command),
            "op: true is not carried"
        );
        assert!(
            menu.dropped
                .contains(&Dropped::Part("someothertrait".into()))
        );
        let parts = menu
            .dropped
            .iter()
            .filter(|d| matches!(d, Dropped::Part(_)))
            .count();
        assert_eq!(parts, 1, "harmless parts are not reported");
    }

    #[test]
    fn rows_it_cannot_place_are_listed_never_guessed() {
        let report = read_saves(SAMPLE).unwrap();
        let reason = |id: &str| {
            report
                .skipped
                .iter()
                .find(|(n, _)| n == id)
                .map(|(_, why)| why.clone())
        };
        assert_eq!(reason("10"), Some(Skipped::WorldIdOnly));
        assert_eq!(reason("9"), Some(Skipped::NotShown));
        assert_eq!(reason("20"), Some(Skipped::UnknownType("unicorn".into())));
        assert_eq!(reason("21"), Some(Skipped::BadPlace));
        assert_eq!(report.rows.len() + report.skipped.len(), 8);
        let named = row(&report, "npc-14");
        assert_eq!(named.skin, None);
        assert!(named.dropped.contains(&Dropped::SkinByName));
    }

    /// A local check against a real save, when one is named. Prints counts only.
    #[test]
    fn a_real_save_when_one_is_named() {
        let Ok(path) = std::env::var("NPC_IMPORT_SAMPLE") else {
            return;
        };
        let report = read_saves(&std::fs::read_to_string(path).unwrap()).unwrap();
        println!(
            "rows {} skipped {}: {:?}",
            report.rows.len(),
            report.skipped.len(),
            report
                .rows
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>()
        );
    }
}
