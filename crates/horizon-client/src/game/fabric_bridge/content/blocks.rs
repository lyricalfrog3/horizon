//! Horizon block declarations for the blocks Fabric mods register in code.
//!
//! The runtime reads each block's facts from its own bytecode
//! ([`RegisteredBlock`]): its class chain, the vanilla methods it overrides,
//! its states and, per state, hardness, light, sounds, occlusion and shapes.
//! A Horizon block copies a vanilla *template* and declares what differs, so
//! the template is chosen from those facts and the vanilla tables:
//!
//! - The block's vanilla class comes first (its first `net.minecraft` class).
//!   A vanilla block of exactly that class and the same states stands in for
//!   it, the one whose shapes and sounds match best, unless the mod's own
//!   classes override what that vanilla class (or one of its vanilla
//!   superclasses below `Block`) does: then the vanilla template would not
//!   behave as the block does, and the next vanilla class up is tried. A
//!   vanilla class no vanilla block has (`HorizontalDirectionalBlock`,
//!   `BaseEntityBlock`) is passed over the same way.
//! - At `Block`, the template is a plain vanilla block (class `Block`, one
//!   state, an item, no fire, fluid, ticks, offset or block entity) with the
//!   same sounds where one has them; the block's own properties are declared
//!   with `state_properties`.
//! - A block whose kept vanilla behaviour needs states no vanilla block of its
//!   class has is left out: a template of other states would not be it.
//!
//! The declared numbers are the block's own: hardness, blast resistance,
//! light, tool need, friction, map colour, sounds, `occludes` and its
//! outline and collision boxes per state. What a declaration cannot say
//! (per-state hardness or light, a block entity, the class's own code) goes
//! into the block's notes for the log.

use hfc_runtime::bridge::{
    BlockContainer, BlockSound, ContainerRead, RegisteredBlock, RegisteredState,
};
use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const VANILLA: &str = "net.minecraft.";
const BLOCK: &str = "net.minecraft.world.level.block.Block";
/// The most boxes a declared shape holds (`horizon_mods`' content fence).
const MAX_BOXES: usize = 16;

/// Overrides that do not make a vanilla template behave otherwise: they build
/// the block (its codec and state definition), or give what the declaration
/// carries from the facts (shapes, and `rotate`/`mirror` over the same
/// properties).
const DATA_METHODS: [&str; 9] = [
    "codec",
    "createBlockStateDefinition",
    "getShape",
    "getCollisionShape",
    "getVisualShape",
    "getOcclusionShape",
    "getInteractionShape",
    "rotate",
    "mirror",
];

/// A block's sounds: volume and pitch bits, then the break, step, place, hit
/// and fall event ids.
type Sound = (u32, u32, [String; 5]);

fn sound_of(s: &BlockSound) -> Sound {
    (
        s.volume.to_bits(),
        s.pitch.to_bits(),
        [
            s.break_sound.clone(),
            s.step.clone(),
            s.place.clone(),
            s.hit.clone(),
            s.fall.clone(),
        ],
    )
}

/// One vanilla state, from the gameplay table.
#[derive(Debug, Clone, PartialEq)]
struct VanillaState {
    sound: Option<Sound>,
    hardness: f32,
    emission: u8,
    can_occlude: bool,
    outline: Vec<[f64; 6]>,
}

#[derive(Debug, Clone)]
struct VanillaBlock {
    /// Simple class names, most derived first (`block-behaviours.json`).
    class: Vec<String>,
    /// Property name to its values.
    properties: BTreeMap<String, BTreeSet<String>>,
    /// State key (`a=x,b=y`, names sorted) to the state.
    states: BTreeMap<String, VanillaState>,
    default: String,
    speed_factor: f32,
    jump_factor: f32,
    /// A plain block a template may be: class `Block` only, one state, an
    /// item, and none of the behaviours a decoration would wrongly take from
    /// it (fire, fluid, random ticks, an offset, a block entity, being
    /// replaceable or unbreakable, a push reaction of its own).
    plain: bool,
}

/// The vanilla blocks templates are chosen from, read from the prepared
/// tables (`facts.json`, `block-behaviours.json`, `gameplay-facts.json`).
#[derive(Debug, Default)]
pub(super) struct Vanilla {
    blocks: BTreeMap<String, VanillaBlock>,
}

#[derive(Deserialize)]
struct FactsFile {
    states: Vec<(String, IgnoredAny, IgnoredAny, IgnoredAny, FactsState)>,
    defaults: BTreeMap<String, u32>,
}
#[derive(Deserialize)]
struct FactsState {
    #[serde(default)]
    properties: BTreeMap<String, String>,
}
#[derive(Deserialize)]
struct BehavioursFile {
    blocks: Vec<BehaviourBlock>,
    #[serde(default)]
    fire: Fire,
}
#[derive(Deserialize)]
struct BehaviourBlock {
    name: String,
    class: Vec<String>,
    #[serde(default)]
    fields: Map<String, Value>,
    #[serde(default)]
    item: Option<String>,
}
#[derive(Deserialize, Default)]
struct Fire {
    #[serde(default, rename = "burnOdds")]
    burn_odds: BTreeMap<String, Value>,
}
#[derive(Deserialize)]
struct GameplayFile {
    states: Vec<GameplayRow>,
}
#[derive(Deserialize)]
struct GameplayRow {
    #[serde(default)]
    hardness: f32,
    #[serde(default)]
    emission: u8,
    #[serde(default)]
    can_occlude: bool,
    #[serde(default)]
    replaceable: bool,
    #[serde(default)]
    ignited_by_lava: bool,
    #[serde(default)]
    block_entity: bool,
    #[serde(default)]
    random_ticks: bool,
    #[serde(default)]
    liquid: bool,
    #[serde(default)]
    offset: bool,
    #[serde(default)]
    push: String,
    #[serde(default = "one")]
    speed_factor: f32,
    #[serde(default = "one")]
    jump_factor: f32,
    #[serde(default)]
    outline: Vec<[f64; 6]>,
    #[serde(default)]
    sound: Option<RowSound>,
}
fn one() -> f32 {
    1.0
}
#[derive(Deserialize)]
struct RowSound {
    volume: f32,
    pitch: f32,
    #[serde(rename = "break")]
    break_sound: SoundId,
    step: SoundId,
    place: SoundId,
    hit: SoundId,
    fall: SoundId,
}
#[derive(Deserialize)]
struct SoundId {
    id: String,
}

/// `a=x,b=y` with the names sorted, as both sides key states.
fn state_key(properties: &BTreeMap<String, String>) -> String {
    properties
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn parse_key(key: &str) -> BTreeMap<String, String> {
    key.split(',')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

const FULL: [f64; 6] = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0];

impl Vanilla {
    /// Reads the prepared tables of `base`.
    pub(super) fn load(base: &Path) -> Result<Vanilla, String> {
        let read = |name: &str| std::fs::read(base.join(name)).map_err(|e| format!("{name}: {e}"));
        let facts: FactsFile =
            serde_json::from_slice(&read("facts.json")?).map_err(|e| format!("facts.json: {e}"))?;
        let behaviours: BehavioursFile = serde_json::from_slice(&read("block-behaviours.json")?)
            .map_err(|e| format!("block-behaviours.json: {e}"))?;
        let gameplay: GameplayFile = serde_json::from_slice(&read("gameplay-facts.json")?)
            .map_err(|e| format!("gameplay-facts.json: {e}"))?;
        Vanilla::from_tables(facts, behaviours, gameplay)
    }

    fn from_tables(
        facts: FactsFile,
        behaviours: BehavioursFile,
        gameplay: GameplayFile,
    ) -> Result<Vanilla, String> {
        if facts.states.len() != gameplay.states.len() {
            return Err("gameplay-facts.json does not match facts.json".into());
        }
        let mut blocks = BTreeMap::new();
        let mut states: BTreeMap<&str, Vec<(String, &GameplayRow, &BTreeMap<String, String>)>> =
            BTreeMap::new();
        for ((name, _, _, _, s), row) in facts.states.iter().zip(&gameplay.states) {
            states.entry(name.as_str()).or_default().push((
                state_key(&s.properties),
                row,
                &s.properties,
            ));
        }
        for b in behaviours.blocks {
            let Some(rows) = states.get(b.name.as_str()) else {
                continue;
            };
            let Some(default_id) = facts.defaults.get(&b.name) else {
                continue;
            };
            let (default_name, _, _, _, default_facts) = &facts.states[*default_id as usize];
            if *default_name != b.name {
                continue;
            }
            let default = state_key(&default_facts.properties);
            let mut properties: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            let mut by_key = BTreeMap::new();
            for (key, row, props) in rows {
                for (k, v) in props.iter() {
                    properties.entry(k.clone()).or_default().insert(v.clone());
                }
                let sound = row.sound.as_ref().map(|s| {
                    (
                        s.volume.to_bits(),
                        s.pitch.to_bits(),
                        [
                            s.break_sound.id.clone(),
                            s.step.id.clone(),
                            s.place.id.clone(),
                            s.hit.id.clone(),
                            s.fall.id.clone(),
                        ],
                    )
                });
                by_key.insert(
                    key.clone(),
                    VanillaState {
                        sound,
                        hardness: row.hardness,
                        emission: row.emission,
                        can_occlude: row.can_occlude,
                        outline: row.outline.clone(),
                    },
                );
            }
            let row = &gameplay.states[*default_id as usize];
            let has_item = b.item.as_deref().is_some_and(|i| i != "minecraft:air");
            let plain = b.class == ["Block"]
                && b.fields.is_empty()
                && properties.is_empty()
                && has_item
                && !behaviours.fire.burn_odds.contains_key(&b.name)
                && !row.replaceable
                && !row.ignited_by_lava
                && !row.block_entity
                && !row.random_ticks
                && !row.liquid
                && !row.offset
                && row.push == "PUSH_PULL"
                && row.hardness >= 0.0
                && row.outline == [FULL];
            blocks.insert(
                b.name,
                VanillaBlock {
                    class: b.class,
                    properties,
                    states: by_key,
                    default,
                    speed_factor: row.speed_factor,
                    jump_factor: row.jump_factor,
                    plain: plain && has_item,
                },
            );
        }
        Ok(Vanilla { blocks })
    }

    /// A vanilla block whose default state sounds as `sound`, plain ones first.
    fn sound_block(&self, sound: &Sound) -> Option<&str> {
        let same = |b: &&VanillaBlock| {
            b.states
                .get(&b.default)
                .and_then(|s| s.sound.as_ref())
                .is_some_and(|s| s == sound)
        };
        self.blocks
            .iter()
            .filter(|(_, b)| b.plain)
            .find(|(_, b)| same(b))
            .or_else(|| self.blocks.iter().find(|(_, b)| same(b)))
            .map(|(n, _)| n.as_str())
    }
}

/// A block's declaration, or why it has none.
#[derive(Debug)]
pub(super) struct Declared {
    pub json: Value,
    /// What the declaration does not reproduce, for the log.
    pub notes: Vec<String>,
}

/// Lower case letters, digits and underscores: what a Horizon state property
/// name or value may be.
fn state_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn simple(class: &str) -> &str {
    class.rsplit('.').next().unwrap_or(class)
}

/// The declaration for `block`, or why it is left out.
pub(super) fn declare(block: &RegisteredBlock, vanilla: &Vanilla) -> Result<Declared, String> {
    if !block.unreadable.is_empty() && block.states.is_empty() {
        return Err(format!(
            "its facts could not be read: {:?}",
            block.unreadable
        ));
    }
    for (name, values) in &block.properties {
        if !state_name(name) || values.iter().any(|v| !state_name(v)) {
            return Err(format!(
                "state property {name} {values:?} is not lower case letters, digits and underscores"
            ));
        }
    }
    let ours = block
        .states
        .get(&block.default)
        .ok_or("its default state has no facts")?;
    let mut notes = Vec::new();

    // The vanilla classes of its chain, most derived first, up to `Block`.
    let chain: Vec<&str> = block
        .class
        .iter()
        .map(String::as_str)
        .filter(|c| c.starts_with(VANILLA))
        .take_while(|c| *c != "net.minecraft.world.level.block.state.BlockBehaviour")
        .collect();
    if !chain.contains(&BLOCK) {
        return Err(format!("its classes {:?} do not reach Block", block.class));
    }
    let wanted: BTreeMap<&str, BTreeSet<&str>> = block
        .properties
        .iter()
        .map(|(k, v)| (k.as_str(), v.iter().map(String::as_str).collect()))
        .collect();
    let needs_item = block.item.as_deref() == Some(block.id.as_str());
    let mut template: Option<&str> = None;
    for (at, class) in chain.iter().enumerate() {
        if *class == BLOCK {
            break;
        }
        let name = simple(class);
        let of_class: Vec<(&String, &VanillaBlock)> = vanilla
            .blocks
            .iter()
            .filter(|(_, v)| v.class.first().is_some_and(|c| c == name))
            .collect();
        if of_class.is_empty() {
            notes.push(format!(
                "{name}: no vanilla block has this class; its behaviour is not reproduced"
            ));
            continue;
        }
        // What the mod's classes override of this class and its vanilla
        // superclasses below `Block`.
        let below: BTreeSet<&str> = chain[at..]
            .iter()
            .take_while(|c| **c != BLOCK)
            .copied()
            .collect();
        let replaced: Vec<&str> = block
            .overrides
            .iter()
            .filter_map(|o| o.rsplit_once('.'))
            .filter(|(c, m)| below.contains(c) && !DATA_METHODS.contains(m))
            .map(|(_, m)| m)
            .collect();
        if !replaced.is_empty() {
            notes.push(format!(
                "{name}: the mod replaces its {}, so a vanilla {name} would not behave as this block does",
                replaced.join(", ")
            ));
            continue;
        }
        let same_states: Vec<(&String, &VanillaBlock)> = of_class
            .into_iter()
            .filter(|(_, v)| {
                v.properties.len() == wanted.len()
                    && v.properties.iter().all(|(k, values)| {
                        wanted.get(k.as_str()).is_some_and(|w| {
                            w.len() == values.len() && values.iter().all(|x| w.contains(x.as_str()))
                        })
                    })
            })
            .collect();
        if same_states.is_empty() {
            return Err(format!(
                "it keeps {name}'s behaviour, and no vanilla {name} has its states {:?}",
                block.properties
            ));
        }
        let score = |v: &VanillaBlock| -> i64 {
            let shapes = block
                .states
                .iter()
                .filter(|(k, s)| {
                    v.states
                        .get(k.as_str())
                        .is_some_and(|t| s.outline.as_deref() == Some(&t.outline[..]))
                })
                .count() as i64;
            let d = v.states.get(&v.default);
            shapes * 1000
                + i64::from(d.and_then(|d| d.sound.as_ref()) == Some(&sound_of(&ours.sound))) * 100
                + i64::from(d.is_some_and(|d| d.hardness == ours.hardness)) * 5
        };
        template = same_states
            .iter()
            .max_by(|a, b| score(a.1).cmp(&score(b.1)).then(b.0.cmp(a.0)))
            .map(|(n, _)| n.as_str());
        break;
    }
    let template = match template {
        Some(t) => t,
        None => {
            let score = |v: &VanillaBlock| -> i64 {
                let d = v.states.get(&v.default);
                i64::from(d.and_then(|d| d.sound.as_ref()) == Some(&sound_of(&ours.sound))) * 100
                    + i64::from(d.is_some_and(|d| d.hardness == ours.hardness)) * 5
                    + i64::from(d.is_some_and(|d| d.emission == ours.emission)) * 2
            };
            vanilla
                .blocks
                .iter()
                .filter(|(_, v)| v.plain)
                .max_by(|a, b| score(a.1).cmp(&score(b.1)).then(b.0.cmp(a.0)))
                .map(|(n, _)| n.as_str())
                .ok_or("no plain vanilla block to copy")?
        }
    };
    let t = &vanilla.blocks[template];
    let theirs = t.states.get(&t.default);

    let mut properties = Map::new();
    if ours.hardness == -1.0 || ours.hardness >= 0.0 {
        properties.insert("hardness".into(), json!(ours.hardness));
    } else {
        notes.push(format!("hardness {} (the template's kept)", ours.hardness));
    }
    if block.explosion_resistance >= 0.0 && block.explosion_resistance.is_finite() {
        properties.insert(
            "explosion_resistance".into(),
            json!(block.explosion_resistance),
        );
    }
    properties.insert("light".into(), json!(ours.emission.min(15)));
    properties.insert("requires_tool".into(), json!(ours.requires_tool));
    if (0.0..=2.0).contains(&block.friction) {
        properties.insert("friction".into(), json!(block.friction));
    } else {
        notes.push(format!("friction {} (the template's kept)", block.friction));
    }
    if ours.map_color_id <= 61 {
        properties.insert("map_color".into(), json!(ours.map_color_id));
    }
    let sound = sound_of(&ours.sound);
    if theirs.and_then(|s| s.sound.as_ref()) != Some(&sound) {
        match vanilla.sound_block(&sound) {
            Some(b) => {
                properties.insert("sounds".into(), json!(b));
            }
            None => notes.push(format!(
                "sounds {} (no vanilla block has them; the template's kept)",
                ours.sound.place
            )),
        }
    }
    let template_occludes = theirs.is_some_and(|s| s.can_occlude);
    if !ours.can_occlude && template_occludes {
        properties.insert("occludes".into(), json!(false));
    } else if ours.can_occlude && !template_occludes {
        notes.push("it occludes, and its template does not".into());
    }
    if (block.speed_factor, block.jump_factor) != (t.speed_factor, t.jump_factor) {
        notes.push(format!(
            "speed and jump factors {} {} (the template's {} {})",
            block.speed_factor, block.jump_factor, t.speed_factor, t.jump_factor
        ));
    }
    // Facts a state may have apart from the default's.
    let mut varying = BTreeSet::new();
    for s in block.states.values() {
        let differs = |what: &'static str, d: bool| d.then_some(what);
        varying.extend(
            [
                differs("hardness", s.hardness != ours.hardness),
                differs("light", s.emission != ours.emission),
                differs("tool need", s.requires_tool != ours.requires_tool),
                differs("map colour", s.map_color_id != ours.map_color_id),
                differs("sounds", s.sound != ours.sound),
                differs("occlusion", s.can_occlude != ours.can_occlude),
            ]
            .into_iter()
            .flatten(),
        );
    }
    if !varying.is_empty() {
        notes.push(format!(
            "per-state {} (the default state's {} used)",
            varying.iter().copied().collect::<Vec<_>>().join(", "),
            block.default
        ));
    }

    let mut file = Map::new();
    file.insert("template".into(), template.into());
    file.insert("properties".into(), Value::Object(properties));
    // Its own properties, beside the template's.
    let mut own = Map::new();
    for (name, values) in &block.properties {
        if t.properties.contains_key(name) {
            continue;
        }
        let set: BTreeSet<&str> = values.iter().map(String::as_str).collect();
        let property = if set == BTreeSet::from(["false", "true"]) {
            json!({"type": "boolean"})
        } else {
            json!({"type": "enum", "values": values})
        };
        own.insert(name.clone(), property);
    }
    if !own.is_empty() {
        file.insert("state_properties".into(), Value::Object(own));
    }
    let default = parse_key(&block.default);
    if !default.is_empty() {
        file.insert("default_state".into(), json!(default));
    }

    // Shapes, when every state's were read.
    let read = |pick: fn(&RegisteredState) -> Option<&Vec<[f64; 6]>>| {
        block
            .states
            .iter()
            .map(|(k, s)| pick(s).map(|b| (k.clone(), b.clone())))
            .collect::<Option<BTreeMap<String, Vec<[f64; 6]>>>>()
    };
    let outline = read(|s| s.outline.as_ref());
    let collision = read(|s| s.collision.as_ref());
    match (outline, collision) {
        (Some(outline), Some(collision)) => {
            let names: Vec<String> = block.properties.keys().cloned().collect();
            let mut merged = 0;
            let mut fit = |boxes: BTreeMap<String, Vec<[f64; 6]>>| {
                boxes
                    .into_iter()
                    .map(|(k, b)| {
                        let (b, m) = fit_boxes(b);
                        merged += usize::from(m);
                        (k, b)
                    })
                    .collect::<BTreeMap<_, _>>()
            };
            let outline = fit(outline);
            let collision = fit(collision);
            if merged > 0 {
                notes.push(format!(
                    "{merged} shape(s) over {MAX_BOXES} boxes merged into fewer, larger boxes"
                ));
            }
            file.insert("shapes".into(), shape_map(&outline, &names));
            if collision != outline {
                file.insert("collision_shapes".into(), shape_map(&collision, &names));
            }
        }
        _ => notes.push("its shapes were not read; the template's are kept".into()),
    }

    match &block.item {
        Some(_) if needs_item => {
            if !block.item_components.is_empty() {
                file.insert(
                    "components".into(),
                    Value::Object(block.item_components.clone()),
                );
            }
        }
        Some(item) => {
            file.insert("item".into(), false.into());
            notes.push(format!(
                "its item {item} has another id than the block, which a declaration cannot give"
            ));
        }
        None => {
            file.insert("item".into(), false.into());
        }
    }
    if block.has_block_entity {
        match &block.container {
            ContainerRead::Container(c) if container_menu_fits(c) => {
                let mut container = Map::new();
                container.insert("slots".into(), c.slots.into());
                container.insert("menu".into(), c.menu_type.clone().into());
                // Vanilla's default name is the block's own; a title that is
                // that key was not set by the mod. A translation key is kept
                // when the mod's `getDisplayName` was a translatable.
                let default = format!("block.{}", block.id.replace(':', "."));
                let title = c
                    .title_key
                    .as_ref()
                    .filter(|k| **k != default && !k.is_empty())
                    .map(|k| ("title_key", k))
                    .or_else(|| {
                        c.title
                            .as_ref()
                            .filter(|t| **t != default && !t.is_empty())
                            .map(|t| ("title", t))
                    });
                if let Some((name, text)) = title {
                    container.insert(name.into(), text.clone().into());
                }
                // Vanilla's `Container.getMaxStackSize` is 64; only a mod's
                // own answer is declared.
                if let Some(max) = c.max_stack_size.filter(|m| *m != 64) {
                    container.insert("max_stack_size".into(), max.into());
                }
                // Every slot is reachable from every face unless the block
                // entity is a `WorldlyContainer` that says otherwise.
                if let Some(faces) = &c.faces {
                    let all: Vec<usize> = (0..c.slots).collect();
                    if faces.iter().any(|slots| *slots != all) {
                        let mut by_face = Map::new();
                        for (name, slots) in ["down", "up", "north", "south", "west", "east"]
                            .into_iter()
                            .zip(faces)
                        {
                            by_face.insert(name.into(), json!(slots));
                        }
                        container.insert("faces".into(), Value::Object(by_face));
                    }
                }
                file.insert("container".into(), Value::Object(container));
                notes.push(
                    "its block entity is a container: slots, menu, items, drops and hoppers are \
                     Horizon's; its ticking and any other code of the entity are not run"
                        .into(),
                );
                notes.push(
                    "its container's per-slot rules (canPlaceItem, and the through-face \
                     canPlaceItemThroughFace/canTakeItemThroughFace) are not run"
                        .into(),
                );
                for note in &c.notes {
                    notes.push(format!("its container: {note}"));
                }
            }
            ContainerRead::Container(c) => notes.push(format!(
                "its container ({} slots, menu {}) has no menu in Horizon; the block stays decoration",
                c.slots, c.menu_type
            )),
            ContainerRead::NotVanillaContainer(class) => notes.push(format!(
                "its block entity {class} is not a vanilla container; the block stays decoration"
            )),
            ContainerRead::Unreadable(why) => notes.push(format!(
                "its block entity's container could not be read ({why}); the block stays decoration"
            )),
            ContainerRead::NoBlockEntity => {
                notes.push("its block entity is not reproduced".into())
            }
        }
    }
    if let Some(own) = block.class.first().filter(|c| !c.starts_with(VANILLA)) {
        notes.push(format!(
            "class {own}: its own code (use, placement, ticks) is not run"
        ));
    }
    Ok(Declared {
        json: Value::Object(file),
        notes,
    })
}

/// Whether Horizon has the menu a container opens, with as many slots:
/// vanilla's slot grids, hopper and furnaces (`containers::MOD_MENUS`).
fn container_menu_fits(c: &BlockContainer) -> bool {
    (1..=256).contains(&c.slots)
        && horizon_client::containers::MOD_MENUS
            .iter()
            .any(|(menu, slots)| *menu == c.menu_type && *slots == c.slots)
}

/// A `shapes` map over the fewest of `names` the boxes depend on: one
/// property's values, several properties' (`properties`), or none.
fn shape_map(by_state: &BTreeMap<String, Vec<[f64; 6]>>, names: &[String]) -> Value {
    let states: Vec<(BTreeMap<String, String>, &Vec<[f64; 6]>)> =
        by_state.iter().map(|(k, b)| (parse_key(k), b)).collect();
    let project = |keep: &[String], props: &BTreeMap<String, String>| -> String {
        keep.iter()
            .map(|n| props.get(n).map_or("", String::as_str).to_owned())
            .collect::<Vec<_>>()
            .join(",")
    };
    // The boxes are a function of `keep` when every state with the same
    // values of it has the same boxes.
    let decides = |keep: &[String]| {
        let mut seen: BTreeMap<String, &Vec<[f64; 6]>> = BTreeMap::new();
        states
            .iter()
            .all(|(props, b)| *seen.entry(project(keep, props)).or_insert(b) == *b)
    };
    let mut keep: Vec<String> = names.to_vec();
    for name in names {
        let without: Vec<String> = keep.iter().filter(|n| *n != name).cloned().collect();
        if decides(&without) {
            keep = without;
        }
    }
    // Every state has the same boxes: one shape for all.
    if keep.is_empty() {
        return json!({"default": states.first().map(|(_, b)| (*b).clone()).unwrap_or_default()});
    }
    let mut values = Map::new();
    for (props, b) in &states {
        let key = if keep.len() == 1 {
            props.get(&keep[0]).cloned().unwrap_or_default()
        } else {
            project(&keep, props)
        };
        values.entry(key).or_insert_with(|| json!(b));
    }
    if keep.len() == 1 {
        json!({"property": keep[0], "values": values})
    } else {
        json!({"properties": keep, "values": values})
    }
}

fn volume(b: &[f64; 6]) -> f64 {
    (b[3] - b[0]) * (b[4] - b[1]) * (b[5] - b[2])
}

fn hull(a: &[f64; 6], b: &[f64; 6]) -> [f64; 6] {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].min(b[2]),
        a[3].max(b[3]),
        a[4].max(b[4]),
        a[5].max(b[5]),
    ]
}

/// At most [`MAX_BOXES`] boxes covering `boxes`: while there are more, the
/// two whose bounding box adds the least volume become that box, so the
/// shape only grows, as little as it can. Whether any merged.
fn fit_boxes(mut boxes: Vec<[f64; 6]>) -> (Vec<[f64; 6]>, bool) {
    let merged = boxes.len() > MAX_BOXES;
    while boxes.len() > MAX_BOXES {
        let mut best = (f64::INFINITY, 0, 1);
        for i in 0..boxes.len() {
            for j in i + 1..boxes.len() {
                let cost =
                    volume(&hull(&boxes[i], &boxes[j])) - volume(&boxes[i]) - volume(&boxes[j]);
                if cost < best.0 {
                    best = (cost, i, j);
                }
            }
        }
        let (_, i, j) = best;
        let joined = hull(&boxes[i], &boxes[j]);
        boxes.swap_remove(j);
        boxes[i] = joined;
    }
    (boxes, merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WOOD: &str = "minecraft:block.wood";
    const STONE: &str = "minecraft:block.stone";

    fn sound(kind: &str) -> BlockSound {
        BlockSound {
            volume: 1.0,
            pitch: 1.0,
            break_sound: format!("{kind}.break"),
            step: format!("{kind}.step"),
            place: format!("{kind}.place"),
            hit: format!("{kind}.hit"),
            fall: format!("{kind}.fall"),
        }
    }

    fn gameplay_row(kind: &str, hardness: f32, occlude: bool, outline: &str) -> String {
        format!(
            r#"{{"hardness":{hardness},"can_occlude":{occlude},"replaceable":false,"ignited_by_lava":false,
               "block_entity":false,"random_ticks":false,"liquid":false,"offset":false,"push":"PUSH_PULL",
               "speed_factor":1.0,"jump_factor":1.0,"outline":{outline},
               "sound":{{"volume":1.0,"pitch":1.0,"break":{{"id":"{kind}.break"}},"step":{{"id":"{kind}.step"}},
               "place":{{"id":"{kind}.place"}},"hit":{{"id":"{kind}.hit"}},"fall":{{"id":"{kind}.fall"}}}}}}"#
        )
    }

    /// A small vanilla: stone and oak planks (plain; the planks burn), a
    /// sea lantern (plain, glass sounds), a glass (not plain) and a red bed
    /// of 16 states. Rows as the prepared tables write them.
    fn vanilla() -> Vanilla {
        let mut facts = Vec::new();
        let mut rows = Vec::new();
        let full = "[[0.0,0.0,0.0,1.0,1.0,1.0]]";
        let mut add = |name: &str, props: &str, row: String| {
            facts.push(format!(
                r#"["{name}",false,false,"",{{"id":"{name}"{props}}}]"#
            ));
            rows.push(row);
        };
        add("minecraft:stone", "", gameplay_row(STONE, 1.5, true, full));
        add(
            "minecraft:oak_planks",
            "",
            gameplay_row(WOOD, 2.0, true, full),
        );
        add(
            "minecraft:glass",
            "",
            gameplay_row("minecraft:block.glass", 0.3, false, full),
        );
        let lantern = gameplay_row("minecraft:block.glass", 0.3, true, full);
        add(
            "minecraft:sea_lantern",
            "",
            lantern.replacen('{', r#"{"emission":15,"#, 1),
        );
        let bed_start = 4;
        for facing in ["north", "south", "west", "east"] {
            for occupied in ["true", "false"] {
                for part in ["head", "foot"] {
                    add(
                        "minecraft:red_bed",
                        &format!(
                            r#","properties":{{"facing":"{facing}","occupied":"{occupied}","part":"{part}"}}"#
                        ),
                        gameplay_row(WOOD, 0.2, false, "[[0.0,0.0,0.0,1.0,0.5625,1.0]]"),
                    );
                }
            }
        }
        // The red bed's default: facing=north, occupied=false, part=foot.
        let facts = format!(
            r#"{{"states":[{}],"defaults":{{"minecraft:stone":0,"minecraft:oak_planks":1,
              "minecraft:glass":2,"minecraft:sea_lantern":3,"minecraft:red_bed":{}}}}}"#,
            facts.join(","),
            bed_start + 3
        );
        let behaviours = r#"{"blocks":[
          {"name":"minecraft:stone","class":["Block"],"fields":{},"item":"minecraft:stone"},
          {"name":"minecraft:oak_planks","class":["Block"],"fields":{},"item":"minecraft:oak_planks"},
          {"name":"minecraft:glass","class":["TransparentBlock","HalfTransparentBlock","Block"],"fields":{},"item":"minecraft:glass"},
          {"name":"minecraft:sea_lantern","class":["Block"],"fields":{},"item":"minecraft:sea_lantern"},
          {"name":"minecraft:red_bed","class":["BedBlock","AbstractBedBlock","HorizontalDirectionalBlock","Block"],
           "fields":{"color":"RED"},"item":"minecraft:red_bed"}],
          "fire":{"burnOdds":{"minecraft:oak_planks":20}}}"#;
        let gameplay = format!(r#"{{"states":[{}]}}"#, rows.join(","));
        Vanilla::from_tables(
            serde_json::from_str(&facts).unwrap(),
            serde_json::from_str(behaviours).unwrap(),
            serde_json::from_str(&gameplay).unwrap(),
        )
        .unwrap()
    }

    fn state(kind: &str, outline: Vec<[f64; 6]>) -> RegisteredState {
        RegisteredState {
            hardness: 1.0,
            emission: 0,
            requires_tool: false,
            map_color_id: 13,
            can_occlude: false,
            sound: sound(kind),
            collision: Some(outline.clone()),
            outline: Some(outline),
        }
    }

    const MOD_BLOCK: [&str; 3] = [
        "com.example.CabinetBlock",
        BLOCK,
        "net.minecraft.world.level.block.state.BlockBehaviour",
    ];

    /// A cabinet as the runtime reports one: a `Block` subclass with its own
    /// `facing`, wood sounds, no occlusion, a box per facing, a block entity.
    fn cabinet() -> RegisteredBlock {
        let boxes = [
            ("north", [0.0, 0.0, 0.3125, 1.0, 1.0, 1.0]),
            ("south", [0.0, 0.0, 0.0, 1.0, 1.0, 0.6875]),
            ("west", [0.3125, 0.0, 0.0, 1.0, 1.0, 1.0]),
            ("east", [0.0, 0.0, 0.0, 0.6875, 1.0, 1.0]),
        ];
        RegisteredBlock {
            id: "ex:cabinet".into(),
            class: MOD_BLOCK.map(String::from).to_vec(),
            has_block_entity: true,
            overrides: vec![
                format!("{BLOCK}.getStateForPlacement"),
                "net.minecraft.world.level.block.state.BlockBehaviour.useWithoutItem".into(),
            ],
            properties: BTreeMap::from([(
                "facing".into(),
                ["north", "south", "west", "east"]
                    .map(String::from)
                    .to_vec(),
            )]),
            default: "facing=north".into(),
            item: Some("ex:cabinet".into()),
            item_components: Map::from_iter([("minecraft:max_stack_size".into(), json!(16))]),
            explosion_resistance: 10.0,
            friction: 0.6,
            speed_factor: 1.0,
            jump_factor: 1.0,
            states: boxes
                .iter()
                .map(|(f, b)| {
                    let mut s = state(WOOD, vec![*b]);
                    s.hardness = 1.5;
                    (format!("facing={f}"), s)
                })
                .collect(),
            container: ContainerRead::Container(BlockContainer {
                slots: 27,
                menu_type: "minecraft:generic_9x3".into(),
                title: Some("Storage".into()),
                title_key: None,
                max_stack_size: Some(8),
                faces: None,
                notes: Vec::new(),
            }),
            unreadable: Vec::new(),
        }
    }

    #[test]
    fn a_plain_block_subclass_copies_a_plain_block_and_declares_its_own_states() {
        let declared = declare(&cabinet(), &vanilla()).unwrap();
        let json = &declared.json;
        // Oak planks share its sounds but burn, which the facts do not say:
        // the plain template is stone (its hardness), with the planks' sounds.
        assert_eq!(json["template"], "minecraft:stone");
        assert_eq!(json["properties"]["sounds"], "minecraft:oak_planks");
        assert_eq!(json["properties"]["hardness"], 1.5);
        assert_eq!(json["properties"]["explosion_resistance"], 10.0);
        assert_eq!(json["properties"]["map_color"], 13);
        assert_eq!(json["properties"]["occludes"], false);
        assert_eq!(
            json["state_properties"],
            json!({"facing": {"type": "enum", "values": ["north", "south", "west", "east"]}})
        );
        assert_eq!(json["default_state"], json!({"facing": "north"}));
        assert_eq!(
            json["shapes"],
            json!({"property": "facing", "values": {
                "north": [[0.0, 0.0, 0.3125, 1.0, 1.0, 1.0]],
                "south": [[0.0, 0.0, 0.0, 1.0, 1.0, 0.6875]],
                "west": [[0.3125, 0.0, 0.0, 1.0, 1.0, 1.0]],
                "east": [[0.0, 0.0, 0.0, 0.6875, 1.0, 1.0]]}})
        );
        // Collision equals the outline, so it is not repeated.
        assert!(json.get("collision_shapes").is_none());
        // Its item is the block's, with the BlockItem's components.
        assert!(json.get("item").is_none());
        assert_eq!(json["components"], json!({"minecraft:max_stack_size": 16}));
        // Its block entity is a 27-slot container with a 9x3 menu, titled by
        // what its `getDisplayName` answered.
        assert_eq!(
            json["container"],
            json!({"slots": 27, "menu": "minecraft:generic_9x3", "title": "Storage",
                   "max_stack_size": 8})
        );
        assert!(
            declared.notes.iter().any(|n| n.contains("is a container")),
            "{:?}",
            declared.notes
        );
        // What the content fence accepts.
        let text = serde_json::to_vec(json).unwrap();
        let files: &[(&str, &[u8])] = &[("data/ex/horizon/block/cabinet.json", &text)];
        let manifest = "id = \"ex\"\nname = \"Ex\"\nversion = \"1\"\napi = \"0.2\"\ncapabilities = [\"registries\"]\n";
        let dir = std::env::temp_dir().join(format!("hfc-block-decl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.hmod");
        std::fs::write(
            &path,
            horizon_mods::manifest::Package::write_files(manifest, None, files).unwrap(),
        )
        .unwrap();
        let package = horizon_mods::manifest::Package::read(&path).unwrap();
        let content = horizon_mods::content::Content::read(&path, &package.manifest).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(content.blocks[0].id, "ex:cabinet");
        assert!(content.blocks[0].item);
        let container = content.blocks[0].container.as_ref().unwrap();
        assert_eq!(
            (
                container.slots,
                container.menu.as_str(),
                container.title.as_deref()
            ),
            (27, "minecraft:generic_9x3", Some("Storage"))
        );
    }

    #[test]
    fn a_container_declares_its_face_slots_and_translation_key() {
        let mut block = cabinet();
        let all: Vec<usize> = (0..27).collect();
        let mut faces: [Vec<usize>; 6] = std::array::from_fn(|_| all.clone());
        // `getSlotsForFace(DOWN)` reaches one slot; the rest stay open.
        faces[0] = vec![1];
        block.container = ContainerRead::Container(BlockContainer {
            slots: 27,
            menu_type: "minecraft:generic_9x3".into(),
            title: Some("Storage".into()),
            title_key: Some("container.ex_cabinet".into()),
            max_stack_size: Some(8),
            faces: Some(faces),
            notes: vec!["getMaxStackSize could not be read".into()],
        });
        let declared = declare(&block, &vanilla()).unwrap();
        let container = &declared.json["container"];
        // A translation key is preferred over the literal it resolved to.
        assert_eq!(container["title_key"], "container.ex_cabinet");
        assert!(container.get("title").is_none());
        assert_eq!(container["max_stack_size"], 8);
        assert_eq!(container["faces"]["down"], json!([1]));
        assert_eq!(container["faces"]["up"], json!(all));
        assert!(
            declared
                .notes
                .iter()
                .any(|n| n.contains("getMaxStackSize could not be read")),
            "{:?}",
            declared.notes
        );
        // What the content fence accepts.
        let text = serde_json::to_vec(&declared.json).unwrap();
        let files: &[(&str, &[u8])] = &[("data/ex/horizon/block/cabinet.json", &text)];
        let manifest = "id = \"ex\"\nname = \"Ex\"\nversion = \"1\"\napi = \"0.2\"\ncapabilities = [\"registries\"]\n";
        let dir = std::env::temp_dir().join(format!("hfc-block-container-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.hmod");
        std::fs::write(
            &path,
            horizon_mods::manifest::Package::write_files(manifest, None, files).unwrap(),
        )
        .unwrap();
        let package = horizon_mods::manifest::Package::read(&path).unwrap();
        let content = horizon_mods::content::Content::read(&path, &package.manifest).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        let container = content.blocks[0].container.as_ref().unwrap();
        assert_eq!(container.max_stack_size, Some(8));
        assert_eq!(container.title_key.as_deref(), Some("container.ex_cabinet"));
        assert_eq!(container.faces.as_ref().unwrap()["down"], vec![1]);
    }

    #[test]
    fn a_container_without_a_set_title_or_a_menu_horizon_has_stays_what_it_can() {
        // A display name that is the block's own key was not set by the mod.
        let mut block = cabinet();
        block.container = ContainerRead::Container(BlockContainer {
            slots: 27,
            menu_type: "minecraft:generic_9x3".into(),
            title: Some("block.ex.cabinet".into()),
            title_key: None,
            max_stack_size: None,
            faces: None,
            notes: Vec::new(),
        });
        let json = declare(&block, &vanilla()).unwrap().json;
        assert_eq!(
            json["container"],
            json!({"slots": 27, "menu": "minecraft:generic_9x3"})
        );

        // A slot count the menu does not show, and a menu Horizon lacks: decoration.
        for (slots, menu) in [(26, "minecraft:generic_9x3"), (4, "minecraft:anvil")] {
            let mut block = cabinet();
            block.container = ContainerRead::Container(BlockContainer {
                slots,
                menu_type: menu.into(),
                title: None,
                title_key: None,
                max_stack_size: None,
                faces: None,
                notes: Vec::new(),
            });
            let declared = declare(&block, &vanilla()).unwrap();
            assert!(declared.json.get("container").is_none(), "{menu}");
            assert!(
                declared
                    .notes
                    .iter()
                    .any(|n| n.contains("stays decoration")),
                "{:?}",
                declared.notes
            );
        }

        // An oven, a clock: their own block entities are not containers.
        for read in [
            ContainerRead::NotVanillaContainer("ex.OvenBlockEntity".into()),
            ContainerRead::Unreadable("getContainerSize() is 0".into()),
        ] {
            let mut block = cabinet();
            block.container = read;
            let declared = declare(&block, &vanilla()).unwrap();
            assert!(declared.json.get("container").is_none());
            assert!(
                declared
                    .notes
                    .iter()
                    .any(|n| n.contains("stays decoration")),
                "{:?}",
                declared.notes
            );
        }
    }

    fn bed(overrides: Vec<String>) -> RegisteredBlock {
        let mut states = BTreeMap::new();
        for facing in ["north", "south", "west", "east"] {
            for occupied in ["true", "false"] {
                for part in ["head", "foot"] {
                    states.insert(
                        format!("facing={facing},occupied={occupied},part={part}"),
                        state(WOOD, vec![[0.0, 0.0, 0.0, 1.0, 0.5, 1.0]]),
                    );
                }
            }
        }
        RegisteredBlock {
            id: "ex:bed".into(),
            class: [
                "com.example.FancyBed",
                "net.minecraft.world.level.block.BedBlock",
                "net.minecraft.world.level.block.AbstractBedBlock",
                "net.minecraft.world.level.block.HorizontalDirectionalBlock",
                BLOCK,
                "net.minecraft.world.level.block.state.BlockBehaviour",
            ]
            .map(String::from)
            .to_vec(),
            has_block_entity: false,
            overrides,
            properties: BTreeMap::from([
                (
                    "facing".into(),
                    ["north", "south", "west", "east"]
                        .map(String::from)
                        .to_vec(),
                ),
                ("occupied".into(), vec!["true".into(), "false".into()]),
                ("part".into(), vec!["head".into(), "foot".into()]),
            ]),
            default: "facing=north,occupied=false,part=head".into(),
            item: Some("ex:bed".into()),
            item_components: Map::new(),
            explosion_resistance: 0.2,
            friction: 0.6,
            speed_factor: 1.0,
            jump_factor: 1.0,
            states,
            container: ContainerRead::NoBlockEntity,
            unreadable: Vec::new(),
        }
    }

    #[test]
    fn a_bed_that_keeps_the_beds_behaviour_copies_a_vanilla_bed() {
        let shape_only = vec!["net.minecraft.world.level.block.BedBlock.getShape".into()];
        let declared = declare(&bed(shape_only), &vanilla()).unwrap();
        let json = &declared.json;
        assert_eq!(json["template"], "minecraft:red_bed");
        // Its states are the bed's: nothing declared, its default picked.
        assert!(json.get("state_properties").is_none());
        assert_eq!(
            json["default_state"],
            json!({"facing": "north", "occupied": "false", "part": "head"})
        );
        // One shape for every state, from the facts.
        assert_eq!(
            json["shapes"],
            json!({"default": [[0.0, 0.0, 0.0, 1.0, 0.5, 1.0]]})
        );
        // The bed does not occlude, so nothing is said about it.
        assert!(json["properties"].get("occludes").is_none());
        assert!(json["properties"].get("sounds").is_none());
    }

    #[test]
    fn a_bed_that_places_itself_its_own_way_is_a_plain_block() {
        let placed = vec![
            "net.minecraft.world.level.block.AbstractBedBlock.setPlacedBy".into(),
            "net.minecraft.world.level.block.BedBlock.getShape".into(),
        ];
        let declared = declare(&bed(placed), &vanilla()).unwrap();
        let json = &declared.json;
        assert_eq!(json["template"], "minecraft:stone");
        assert_eq!(
            json["state_properties"]["occupied"],
            json!({"type": "boolean"})
        );
        assert_eq!(
            json["state_properties"]["part"]["values"],
            json!(["head", "foot"])
        );
        assert!(
            declared
                .notes
                .iter()
                .any(|n| n.contains("BedBlock") && n.contains("setPlacedBy")),
            "{:?}",
            declared.notes
        );
    }

    #[test]
    fn a_kept_behaviour_without_matching_vanilla_states_is_left_out() {
        let mut odd = bed(Vec::new());
        odd.properties.remove("occupied");
        odd.states = odd
            .states
            .into_iter()
            .filter(|(k, _)| k.contains("occupied=false"))
            .map(|(k, s)| (k.replace(",occupied=false", ""), s))
            .collect();
        odd.default = "facing=north,part=head".into();
        let e = declare(&odd, &vanilla()).unwrap_err();
        assert!(e.contains("BedBlock"), "{e}");
    }

    #[test]
    fn shapes_depend_on_the_fewest_properties_and_collision_stands_apart() {
        let mut lamp = cabinet();
        lamp.id = "ex:lamp".into();
        lamp.properties
            .insert("lit".into(), vec!["true".into(), "false".into()]);
        let mut states = BTreeMap::new();
        for facing in ["north", "south", "west", "east"] {
            for lit in ["true", "false"] {
                let height = if lit == "true" { 1.0 } else { 0.5 };
                let mut s = state(STONE, vec![[0.0, 0.0, 0.0, 1.0, height, 1.0]]);
                s.collision = Some(Vec::new());
                s.emission = if lit == "true" { 15 } else { 0 };
                states.insert(format!("facing={facing},lit={lit}"), s);
            }
        }
        lamp.states = states;
        lamp.default = "facing=north,lit=false".into();
        let declared = declare(&lamp, &vanilla()).unwrap();
        let json = &declared.json;
        assert_eq!(json["template"], "minecraft:stone");
        assert!(json["properties"].get("sounds").is_none());
        assert_eq!(json["state_properties"]["lit"], json!({"type": "boolean"}));
        assert_eq!(
            json["shapes"],
            json!({"property": "lit", "values": {
                "true": [[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]],
                "false": [[0.0, 0.0, 0.0, 1.0, 0.5, 1.0]]}})
        );
        assert_eq!(json["collision_shapes"], json!({"default": []}));
        // Light differs per state: the default's (unlit) is declared, and said.
        assert_eq!(json["properties"]["light"], 0);
        assert!(
            declared.notes.iter().any(|n| n.contains("per-state light")),
            "{:?}",
            declared.notes
        );
    }

    #[test]
    fn more_than_sixteen_boxes_merge_into_a_cover() {
        let boxes: Vec<[f64; 6]> = (0..20)
            .map(|i| {
                let x = f64::from(i) / 20.0;
                [x, 0.0, 0.0, x + 0.05, 0.1, 0.1]
            })
            .collect();
        let (fitted, merged) = fit_boxes(boxes.clone());
        assert!(merged);
        assert_eq!(fitted.len(), MAX_BOXES);
        // Every original box lies inside one of the fitted ones.
        for b in &boxes {
            assert!(
                fitted
                    .iter()
                    .any(|f| (0..3).all(|i| f[i] <= b[i] && b[i + 3] <= f[i + 3])),
                "{b:?}"
            );
        }
        assert_eq!(fit_boxes(boxes[..3].to_vec()), (boxes[..3].to_vec(), false));
    }
}
