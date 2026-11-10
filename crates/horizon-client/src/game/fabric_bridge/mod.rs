//! Bridge connecting Horizon's Game to the dynamic Fabric compatibility layer.
//!
//! `mod.rs` owns the runtime singleton and mod loading; the submodules hold the
//! GUI replay, menu widgets, texture upload and key numbering.

use super::{Game, GameLevel};
use horizon_client::items::ItemStack;
use horizon_client::text::Text;
use horizon_core::position::BlockPos;
use horizon_core::ticks::{ScheduleResult, TickPriority};
use horizon_world::level::PlacementContext;
use horizon_world::registry::BlockStateId;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

mod block_entity_nbt;
mod content;
mod gui;
mod keys;
mod menu;
mod textures;

pub(super) use content::fabric_content_mods;
pub(super) use gui::{
    apply_fabric_host_requests, fabric_screen_char, fabric_screen_close_current,
    fabric_screen_draws, fabric_screen_key, fabric_screen_mouse, fabric_screen_mouse_moved,
    fabric_screen_open, fabric_screen_scroll, replay_fabric_draws, translate_draw,
};
pub(super) use menu::{draw_menu_widgets, menu_widget_press};

#[allow(unused_imports)]
use winit::keyboard::KeyCode;

/// Jars Options > Mods switched off: not loaded, but still listed so the
/// player can turn them back on.
fn switched_off_mods() -> &'static Mutex<Vec<hfc_runtime::bridge::JarSummary>> {
    static OFF: OnceLock<Mutex<Vec<hfc_runtime::bridge::JarSummary>>> = OnceLock::new();
    OFF.get_or_init(|| Mutex::new(Vec::new()))
}

/// The prepared resources (`registries.json`, tags) server-side mods read
/// block and item ids from; set before the bridge starts.
static RESOURCES: OnceLock<PathBuf> = OnceLock::new();

/// Set while the bridge itself breaks a block for a mod, so the break does not
/// raise the block-break event again.
static BREAKING_FOR_MOD: AtomicBool = AtomicBool::new(false);

pub(super) fn set_fabric_resources(dir: PathBuf) {
    let _ = RESOURCES.set(dir);
}

static BRIDGE: OnceLock<Mutex<hfc_runtime::bridge::FabricBridge>> = OnceLock::new();

/// The bridge if something already started it. A world hook that runs for
/// every block placed asks this, so a game without Fabric mods never pays for
/// starting the runtime on a placement.
fn started_fabric_bridge() -> Option<&'static Mutex<hfc_runtime::bridge::FabricBridge>> {
    BRIDGE.get()
}

fn get_fabric_bridge() -> &'static Mutex<hfc_runtime::bridge::FabricBridge> {
    BRIDGE.get_or_init(|| {
        let mut bridge = hfc_runtime::bridge::FabricBridge::new();
        add_vanilla_classes(&mut bridge);
        let dirs = [
            Game::game_dir().join(horizon_client::mod_list::DIR),
            horizon_client::logging::player_dir().join(horizon_client::mod_list::DIR),
            PathBuf::from("horizon-mods"),
            PathBuf::from("test-mods"),
        ];
        let mut loaded = BTreeSet::new();
        let mut switched_off = Vec::new();
        // Options > Mods lists a Fabric mod as on until its line says
        // `mod.<id>:false`; a mod switched off is not loaded, so none of its
        // entrypoints, key mappings or menu widgets exist. The file is read
        // once at startup, so a change applies at the next launch.
        let disabled = horizon_client::mod_list::ModList::load(
            &horizon_client::logging::shared_dir(&Game::game_dir()),
        )
        .disabled;
        for dir in dirs {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                let mut jars: Vec<_> = entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jar"))
                    .collect();
                jars.sort();
                for jar in jars {
                    if jar
                        .file_name()
                        .is_some_and(|name| loaded.insert(name.to_os_string()))
                    {
                        if let Some(summary) = hfc_runtime::bridge::FabricBridge::jar_summary(&jar)
                            && disabled.contains(&summary.id)
                        {
                            log::info!(
                                "Fabric mod {} is turned off: not loading {}",
                                summary.id,
                                jar.display()
                            );
                            switched_off.push(summary);
                            continue;
                        }
                        log::info!("Loading Fabric mod jar: {}", jar.display());
                        if let Err(e) = bridge.load_mod_jar(&jar) {
                            log::warn!("Failed to load Fabric mod jar {}: {e}", jar.display());
                        }
                    }
                }
            }
        }
        if let Some(resources) = RESOURCES.get() {
            bridge.load_registries(resources);
        }
        bridge.set_game_dir(Some(Game::game_dir()));
        if let Err(e) = bridge.init() {
            log::warn!("Failed to initialize FabricBridge: {e}");
        }
        bridge.server_starting();
        let report = bridge.class_closure();
        log::info!(
            "Fabric class closure: {} resolved, {} missing",
            report.resolved.len(),
            report.missing.len()
        );
        for name in report.missing.iter().take(40) {
            log::warn!("Fabric runtime has no class for {name}");
        }
        *switched_off_mods()
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = switched_off;
        Mutex::new(bridge)
    })
}

/// Register the player's own Minecraft installation: the client jar, every
/// library it lists, and the JDK image the launcher downloaded.
///
/// Classes are then loaded from the same bytes a JVM would read, on demand,
/// instead of the runtime carrying a hand-written copy of each one.
fn add_vanilla_classes(bridge: &mut hfc_runtime::bridge::FabricBridge) {
    let Some(root) = minecraft_root() else {
        log::info!("No Minecraft installation found for the Fabric classpath");
        return;
    };

    if let Some(client) = newest_version_jar(&root) {
        let _ = bridge.add_jar(&client).map_err(|e| {
            log::warn!("Failed to index {}: {e}", client.display());
        });
    }

    let mut libraries = Vec::new();
    collect_jars(&root.join("libraries"), &mut libraries);
    libraries.sort();
    for jar in &libraries {
        if let Err(e) = bridge.add_jar(jar) {
            log::warn!("Failed to index {}: {e}", jar.display());
        }
    }

    if let Some(image) = find_jimage(&root.join("runtime")) {
        let _ = bridge.add_jimage(&image).map_err(|e| {
            log::warn!("Failed to index {}: {e}", image.display());
        });
    }
}

fn minecraft_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::var_os("HORIZON_MINECRAFT_DIR") {
        candidates.push(PathBuf::from(dir));
    }
    if let Some(home) = home {
        candidates.push(home.join(".local/share/horizon-launcher/minecraft"));
        candidates.push(home.join(".minecraft"));
    }
    candidates.into_iter().find(|dir| dir.is_dir())
}

fn newest_version_jar(root: &std::path::Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root.join("versions")).ok()?;
    let mut jars: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| {
            entry
                .path()
                .join(format!("{}.jar", entry.file_name().to_string_lossy()))
        })
        .filter(|jar| jar.is_file())
        .collect();
    jars.sort();
    jars.pop()
}

fn collect_jars(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jars(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("jar") {
            out.push(path);
        }
    }
}

fn find_jimage(dir: &std::path::Path) -> Option<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_jimage(&path) {
                return Some(found);
            }
        } else if path.file_name().and_then(|s| s.to_str()) == Some("modules") {
            return Some(path);
        }
    }
    None
}

/// Sync active Fabric mod translations into Horizon's global Language dictionary.
pub(super) fn sync_fabric_translations() {
    let current_code = horizon_client::lang::code();
    if let Ok(mut bridge) = get_fabric_bridge().lock() {
        bridge.reload_translations(&current_code);
        bridge.extend_translations(horizon_client::lang::entries());
        let entries = bridge.translations().clone();
        horizon_client::lang::extend(entries);
    }
}

/// Initialize the Fabric bridge early during client startup.
pub(super) fn init_fabric_bridge() {
    let _ = get_fabric_bridge();
    sync_fabric_translations();
}

/// Returns the metadata of all loaded Fabric mods.
pub(super) fn loaded_fabric_mods() -> Vec<hfc_runtime::FabricModMetadata> {
    if let Ok(bridge) = get_fabric_bridge().lock() {
        bridge.loaded_mods().to_vec()
    } else {
        Vec::new()
    }
}

/// Populates the given ModList with loaded Fabric mods.
pub(super) fn populate_fabric_mods(mods: &mut horizon_client::mod_list::ModList) {
    for meta in loaded_fabric_mods() {
        if !meta.is_nested && !mods.installed.iter().any(|m| m.id == meta.id) {
            mods.installed.push(horizon_client::mod_list::Installed {
                id: meta.id,
                title: format!("{} {}", meta.name, meta.version),
                description: meta.description,
                problem: None,
                dev: false,
                native: Some(horizon_client::mod_list::NativePage::default()),
            });
        }
    }
    let off = switched_off_mods()
        .lock()
        .map(|off| off.clone())
        .unwrap_or_default();
    for meta in off {
        if !mods.installed.iter().any(|m| m.id == meta.id) {
            mods.installed.push(horizon_client::mod_list::Installed {
                id: meta.id,
                title: format!("{} {}", meta.name, meta.version),
                description: meta.description,
                problem: None,
                dev: false,
                native: Some(horizon_client::mod_list::NativePage::default()),
            });
        }
    }
}

struct HostBridgeAdapter<'a, 'b>(&'a mut Game<'b>);

impl hfc_runtime::bridge::HostBridge for HostBridgeAdapter<'_, '_> {
    fn block(&self, pos: [i32; 3]) -> String {
        self.0.native_block(pos)
    }
    fn block_property(&self, pos: [i32; 3], key: &str) -> Option<String> {
        self.0.native_block_property(pos, key)
    }
    fn set_block_property(&mut self, pos: [i32; 3], key: &str, value: &str) -> bool {
        self.0.native_set_block_property(pos, key, value)
    }
    fn give(&mut self, item: &str, count: u32) -> Result<(), String> {
        self.0.native_give(item, count)
    }
    fn held_item(&self) -> Option<String> {
        self.0.held().map(|s| s.item.to_string())
    }
    fn inventory(&self) -> Vec<Option<(String, i32)>> {
        // Vanilla's `Inventory` order: the 36 main slots (hotbar last), then
        // the off hand. A screen's player slots are laid out in that order.
        self.0
            .inventory
            .iter()
            .map(|stack| {
                stack
                    .as_ref()
                    .map(|stack| (stack.item.to_string(), stack.count as i32))
                    .filter(|(item, count)| *count > 0 && item != "minecraft:air")
            })
            .collect()
    }
    fn off_hand_item(&self) -> Option<String> {
        self.0.offhand.as_ref().map(|s| s.item.to_string())
    }
    fn block_properties(&self, pos: [i32; 3]) -> Vec<(String, String)> {
        let state = self.0.world.block(pos[0], pos[1], pos[2]);
        self.0
            .pack
            .assets
            .registry
            .state(state)
            .map(|s| {
                s.properties
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }
    fn held_enchantments(&self) -> Vec<(String, i32)> {
        self.0
            .held()
            .map(|s| {
                s.enchantments()
                    .iter()
                    .map(|(id, level)| (id.to_string(), *level as i32))
                    .collect()
            })
            .unwrap_or_default()
    }
    fn break_block(&mut self, pos: [i32; 3]) -> bool {
        if self.0.native_block(pos) == "minecraft:air" {
            return false;
        }
        BREAKING_FOR_MOD.store(true, Ordering::SeqCst);
        let broken = self.0.break_block(pos).is_ok();
        BREAKING_FOR_MOD.store(false, Ordering::SeqCst);
        broken
    }
    fn drop_item(&mut self, pos: [i32; 3], item: &str, count: u32) {
        let _ = self.0.native_give(item, count);
        self.0.pop_resource(pos, ItemStack::new(item, count));
    }
    fn remove_block(&mut self, pos: [i32; 3], drop: bool) -> bool {
        if self.0.native_block(pos) == "minecraft:air" {
            return false;
        }
        if drop {
            BREAKING_FOR_MOD.store(true, Ordering::SeqCst);
            let broken = self.0.break_block(pos).is_ok();
            BREAKING_FOR_MOD.store(false, Ordering::SeqCst);
            return broken;
        }
        let dimension = self.0.dim.id().to_string();
        let Some(air) = self.0.pack.assets.registry.default_state("minecraft:air") else {
            return false;
        };
        self.0.native_set_block(
            &dimension,
            pos,
            air,
            horizon_world::level::flags::UPDATE_ALL,
        )
    }
    fn can_remove_blocks(&self) -> bool {
        true
    }
    fn schedule_block_tick(&mut self, pos: [i32; 3], block: &str, delay: u32) {
        self.0.schedule_fabric_tick(pos, block, delay);
    }
    fn particle(&mut self, id: &str, at: [f64; 3], count: u32, spread: [f64; 3], speed: f64) {
        use horizon_client::particles::{ParticleOptions, ParticleType};
        let Some(ty) = ParticleType::from_name(id) else {
            log::debug!("Fabric: no particle type {id}");
            return;
        };
        let Some(lan) = self.0.lan.as_ref() else {
            return;
        };
        super::lan_particles::send_hub_level_particles(
            &lan.hub,
            &ParticleOptions::simple(ty),
            at,
            spread,
            speed,
            count,
            None,
        );
    }
}

impl GameLevel<'_> {
    /// A due scheduled tick of a block a Fabric mod registered in code. Native
    /// mods had the first chance; queue it for the runtime and keep vanilla's
    /// behaviour table from ticking it.
    pub(super) fn fabric_due_tick(
        &mut self,
        pos: BlockPos,
        state: BlockStateId,
        handled: &mut bool,
    ) {
        let Some(name) = self
            .registry
            .state(state)
            .map(|state| state.name.to_string())
        else {
            return;
        };
        let Some(bridge) = started_fabric_bridge() else {
            return;
        };
        let registered = bridge
            .lock()
            .map(|bridge| bridge.is_registered_block(&name))
            .unwrap_or(false);
        if registered {
            self.fabric_due_ticks.push((pos, state));
            *handled = true;
        }
    }
}

impl Game<'_> {
    /// `PlayerBlockBreakEvents.BEFORE` for the block at `pos`; a listener that
    /// returns false cancels the break. Fabric server mods that break more
    /// blocks (Veinminer) do it through the host's own `break_block`.
    pub(super) fn fabric_destroy_block(&mut self, pos: [i32; 3], canceled: &mut bool) {
        if *canceled || BREAKING_FOR_MOD.load(Ordering::SeqCst) {
            return;
        }
        let crouching = self.keys.contains(&KeyCode::ShiftLeft);
        if let Ok(mut bridge) = get_fabric_bridge().lock() {
            let mut adapter = HostBridgeAdapter(self);
            if bridge.block_break_before(&mut adapter, pos, crouching, 8)
                == hfc_runtime::bridge::Flow::Cancel
            {
                *canceled = true;
            }
        }
    }

    pub(super) fn fabric_right_click_block(
        &mut self,
        pos: [i32; 3],
        face: [i32; 3],
        hit: [f64; 3],
        sneaking: bool,
        off_hand: bool,
        canceled: &mut bool,
    ) {
        if *canceled {
            return;
        }

        let event = hfc_runtime::bridge::RightClickBlockEvent {
            pos,
            face,
            hit,
            sneaking,
            off_hand,
        };
        if let Ok(mut bridge) = get_fabric_bridge().lock() {
            let mut adapter = HostBridgeAdapter(self);
            if bridge.right_click_block(&mut adapter, &event) == hfc_runtime::bridge::Flow::Cancel {
                *canceled = true;
            }
        }
        // A block that makes a block entity has one once it is used, even if
        // its use did not run: vanilla's chunk holds it either way.
        self.fabric_ensure_block_entity(pos);
        // The menu its own code opened (`Player.openMenu`) is shown over the
        // world; its slots are the mod's own inventory, read and written
        // through this bridge so a tick sees what the player put in.
        let opened = match get_fabric_bridge().lock() {
            Ok(mut bridge) => bridge.take_opened_menu(),
            Err(_) => None,
        };
        if let Some((block, menu)) = opened {
            self.open_fabric_menu(pos, &block, menu);
        }
    }

    /// Show the menu a Fabric block's own code opened (`Player.openMenu`).
    ///
    /// When the client knows the menu type's own screen (a `MenuScreens`
    /// entry), the menu's `setScreen` already installed it (the mod's real
    /// screen, e.g. a pot's `CookingPotScreen`), built by the client's
    /// registered constructor for the menu's `getType`. The host then follows
    /// that real screen instead of drawing a `MOD_MENUS` grid, and leaves the
    /// mod's block entity as the source of its items: the real screen writes
    /// its own inventory on a click, and the block's own `saveAdditional`
    /// (run by [`Game::fabric_save_block_entities`]) persists it.
    ///
    /// When the client has no screen for the menu type, the host draws a
    /// vanilla grid for the slot count; its slots are the mod's own inventory,
    /// kept in step with it: a click writes the block entity's item list, and
    /// [`Game::fabric_sync_menu_items`] copies that into the mod's inventory
    /// before its code runs.
    fn open_fabric_menu(
        &mut self,
        pos: [i32; 3],
        block: &str,
        menu: hfc_runtime::bridge::OpenedMenu,
    ) {
        // The menu's `setScreen` installed the mod's own screen (a
        // `MenuScreens` entry for the menu type): follow it rather than draw a
        // host grid, and leave the mod's block entity as the source of its
        // items. Slot count is irrelevant here: the real screen renders itself.
        if get_fabric_bridge()
            .lock()
            .is_ok_and(|bridge| bridge.has_screen())
        {
            log::debug!("Fabric: {block}'s menu opened its own screen; the host follows it");
            self.fabric_follow_screen();
            self.right = false;
            return;
        }
        let slots = menu.slots;
        if slots == 0 {
            return;
        }
        // The host screen draws a vanilla grid for the slot count; a menu with
        // no such grid is not shown rather than drawn wrongly.
        let Some((menu_id, grid)) = crate::containers::MOD_MENUS
            .into_iter()
            .find(|(_, size)| *size == slots)
        else {
            log::warn!("Fabric: {block}'s menu has {slots} slots, which no host menu draws");
            return;
        };
        let key = (pos[0], pos[1], pos[2]);
        let state = self.world.block(pos[0], pos[1], pos[2]);
        let inputs = menu.input.clone();
        let container = std::sync::Arc::new(crate::containers::ModContainer {
            slots: grid,
            max_stack: 64,
            faces: std::array::from_fn(|_| (0..grid).collect()),
            inputs: inputs.clone(),
            outputs: (0..grid).filter(|i| !inputs.contains(i)).collect(),
            menu: Some(menu_id.to_string()),
        });
        let mut table = self.world.block_entities.mod_containers().clone();
        table.insert(state.0, std::sync::Arc::clone(&container));
        self.world.block_entities.set_mod_containers(table);
        let mut items: Vec<Option<ItemStack>> = menu
            .items
            .iter()
            .map(|slot| {
                slot.as_ref()
                    .map(|(item, count)| ItemStack::new(item, *count as u32))
            })
            .collect();
        items.resize(grid, None);
        self.world.block_entities.insert(
            key,
            crate::containers::BlockEntity::Mod(Box::new(
                crate::containers::ModBlockEntityFields {
                    id: block.to_owned(),
                    items,
                    ..Default::default()
                }
                .into(),
            )),
        );
        self.container = Some(key);
        self.open_container_slots = Some((key, grid));
        self.fabric_menu = Some((key, block.to_owned(), menu.title, inputs));
        self.screen = crate::game::Screen::Inventory;
        self.capture(false);
        self.right = false;
    }

    /// A player placed a Fabric block: run the block's own `onPlace` and
    /// `setPlacedBy` over the world around it, and set the blocks they set (a
    /// bed's other cell) through the host, as `Level.setBlock` from the
    /// mod's code would. Server-side only; `click` is the click that placed it.
    pub(super) fn fabric_block_placed(
        &mut self,
        click: &horizon_world::level::PlaceContext,
        target: [i32; 3],
        state: BlockStateId,
    ) {
        use hfc_runtime::bridge::PlacedBy;
        if self.serving.is_none() && self.options.dedicated.is_none() {
            return;
        }
        let Some(bridge) = started_fabric_bridge() else {
            return;
        };
        let Some(placed) = self.pack.assets.registry.state(state) else {
            return;
        };
        let block = placed.name.to_string();
        let properties: std::collections::BTreeMap<String, String> = placed
            .properties
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let clicked = [
            click.clicked_pos.x,
            click.clicked_pos.y,
            click.clicked_pos.z,
        ];
        let request = hfc_runtime::bridge::PlacementRequest {
            block: block.clone(),
            pos: target,
            hit_pos: clicked,
            face: click.face.normal(),
            hit: [click.hit.x, click.hit.y, click.hit.z],
            inside: click.inside,
            yaw: click.yaw,
            pitch: click.pitch,
            player_pos: self
                .player
                .as_ref()
                .map_or([0.0; 3], |p| [p.position.x, p.position.y, p.position.z]),
            sneaking: click.sneaking,
            off_hand: self.hand == crate::first_person::Hand::Off,
            item: click.item.clone(),
            replace_clicked: clicked == target,
        };
        // The bridge is released before the blocks are set: setting one can
        // raise events that lock it again.
        let result = match bridge.lock() {
            Ok(mut bridge) => {
                let adapter = HostBridgeAdapter(self);
                let placed = bridge.block_placed(&adapter, &request, &properties);
                // A block that makes a block entity has one from the start, as
                // vanilla's `Level.setBlock` does: its own code (`setPlacedBy`,
                // a later use or tick) reads it.
                bridge.ensure_block_entity(&adapter, target, &block);
                placed
            }
            Err(_) => return,
        };
        match result {
            PlacedBy::NotOverridden => {}
            PlacedBy::Failed(error) => {
                log::warn!("Fabric: {block}'s setPlacedBy failed: {error}");
            }
            PlacedBy::Changes(changes) => {
                let dimension = self.dim.id().to_string();
                for change in changes {
                    let Some(default) = self.pack.assets.registry.default_state(&change.block)
                    else {
                        log::warn!(
                            "Fabric: {block} set {}, which is not a block here",
                            change.block
                        );
                        continue;
                    };
                    let Some(id) =
                        placed_state(self.pack.assets.registry, default, &change.properties)
                    else {
                        log::warn!(
                            "Fabric: {block} set {} {:?}, which is none of its states",
                            change.block,
                            change.properties
                        );
                        continue;
                    };
                    self.native_set_block(
                        &dimension,
                        change.pos,
                        id,
                        horizon_world::level::flags::UPDATE_ALL,
                    );
                }
            }
        }
    }

    /// Schedule a Fabric block's tick on the level's vanilla scheduler, which
    /// saves it with the chunk, follows the dimension and deduplicates. A
    /// position outside the loaded chunks is dropped, as vanilla does.
    pub(super) fn schedule_fabric_tick(&mut self, pos: [i32; 3], block: &str, delay: u32) {
        let Some(state) = self.pack.assets.registry.default_state(block) else {
            log::warn!("Fabric: cannot schedule a tick of {block}: no such block");
            return;
        };
        let behaviour = self.res.behaviours().block(state);
        let at = BlockPos::new(pos[0], pos[1], pos[2]);
        let time = self.world.game_time;
        if self.block_ticks.schedule_block(
            time,
            at,
            behaviour,
            delay.max(1) as i32,
            TickPriority::Normal,
        ) == ScheduleResult::MissingChunk
        {
            log::debug!("Fabric: the tick of {block} at {pos:?} is outside the loaded chunks");
        }
    }

    /// Run the mod block ticks the vanilla scheduler made due this game tick.
    /// Each runs the block's own `tick` over the world around it; what it
    /// sets, schedules and sends is applied through the host. Unlike vanilla,
    /// at most 10 ms of Fabric ticks run in one game tick: a mod that
    /// reschedules many blocks at once has the rest deferred one tick.
    pub(super) fn tick_fabric_blocks(&mut self) {
        let due = std::mem::take(&mut self.fabric_due_ticks);
        if due.is_empty() {
            return;
        }
        if self.serving.is_none() && self.options.dedicated.is_none() {
            return;
        }
        let Some(bridge) = started_fabric_bridge() else {
            return;
        };
        let Ok(mut bridge) = bridge.lock() else {
            return;
        };
        let started = std::time::Instant::now();
        let mut deferred = Vec::new();
        for (index, (pos, state)) in due.into_iter().enumerate() {
            if index > 0 && started.elapsed() > std::time::Duration::from_millis(10) {
                deferred.push((pos, state));
                continue;
            }
            let Some(block) = self
                .pack
                .assets
                .registry
                .state(state)
                .map(|state| state.name.to_string())
            else {
                continue;
            };
            // The block may have been broken or replaced since the tick was
            // scheduled: then its tick no longer applies.
            if self.native_block([pos.x, pos.y, pos.z]) != block {
                continue;
            }
            let mut adapter = HostBridgeAdapter(self);
            if let Some(effects) = bridge.tick_block(&adapter, [pos.x, pos.y, pos.z], &block) {
                bridge.apply_block_effects(&mut adapter, &effects);
            }
        }
        for (pos, state) in deferred {
            if let Some(block) = self
                .pack
                .assets
                .registry
                .state(state)
                .map(|state| state.name.to_string())
            {
                self.schedule_fabric_tick([pos.x, pos.y, pos.z], &block, 1);
            }
        }
    }

    /// Whether a Fabric mod registered `name` in code and its block makes a
    /// block entity: the host gives such a block a `ModBlockEntity` of its own
    /// id, which saves the runtime's tree under `HorizonFabric`.
    pub(super) fn fabric_has_block_entity(&self, name: &str) -> bool {
        let Some(bridge) = started_fabric_bridge() else {
            return false;
        };
        let Ok(bridge) = bridge.lock() else {
            return false;
        };
        bridge.block_entity_declared(name)
    }

    /// Make the block entity a Fabric block's own code expects at `pos`: the
    /// mod's real `BlockEntity` object, kept in the runtime, so its own use and
    /// tick code can read it (`Level.getBlockEntity` answers it).
    /// Write every kept Fabric block entity's own saved state into the world's
    /// block entities before a chunk is snapshotted, so the pot's contents and
    /// cook progress are in the world it saves.
    pub(super) fn fabric_save_block_entities(&mut self) {
        if self.serving.is_none() && self.options.dedicated.is_none() {
            return;
        }
        let Some(bridge) = started_fabric_bridge() else {
            return;
        };
        let kept: Vec<([i32; 3], String)> = match bridge.lock() {
            Ok(bridge) => bridge.kept_block_entity_positions(),
            Err(_) => return,
        };
        for (pos, block) in kept {
            if self
                .pack
                .assets
                .registry
                .state(self.world.block(pos[0], pos[1], pos[2]))
                .map(|state| state.name.as_str())
                != Some(block.as_str())
            {
                continue;
            }
            let Some(values) = self.fabric_save_block_entity(pos) else {
                continue;
            };
            let key = (pos[0], pos[1], pos[2]);
            // `ModBlockEntity` is read and changed through its fields
            // (`DerefMut`), so a write gives it a new revision.
            if let Some(crate::containers::BlockEntity::Mod(entity)) =
                self.world.block_entities.get_mut(&key)
            {
                entity.nbt.insert(
                    block_entity_nbt::KEY.into(),
                    horizon_world::nbt::Tag::Compound(block_entity_nbt::to_nbt(&values)),
                );
            }
        }
    }

    pub(super) fn fabric_ensure_block_entity(&mut self, pos: [i32; 3]) {
        let Some(block) = self
            .pack
            .assets
            .registry
            .state(self.world.block(pos[0], pos[1], pos[2]))
            .map(|state| state.name.to_string())
        else {
            return;
        };
        let Some(bridge) = started_fabric_bridge() else {
            return;
        };
        let Ok(mut bridge) = bridge.lock() else {
            return;
        };
        if !bridge.is_registered_block(&block) {
            return;
        }
        let mut adapter = HostBridgeAdapter(self);
        bridge.ensure_block_entity(&adapter, pos, &block);
    }

    /// Tick the Fabric blocks' block entities: each kept entity whose block
    /// declares a ticker runs its own `cookingTick`-style tick, and what it
    /// changed is applied through the host.
    pub(super) fn tick_fabric_block_entities(&mut self) {
        if self.serving.is_none() && self.options.dedicated.is_none() {
            return;
        }
        let Some(bridge) = started_fabric_bridge() else {
            return;
        };
        let Ok(mut bridge) = bridge.lock() else {
            return;
        };
        if bridge.kept_block_entity_count() == 0 {
            return;
        }
        // What the player put in an open Fabric menu goes into the mod's own
        // inventory first, so this tick reads it (the pot's cooking tick takes
        // its ingredients from there).
        for (pos, block) in self.fabric_menu_items() {
            let key = (pos[0], pos[1], pos[2]);
            let Some(crate::containers::BlockEntity::Mod(entity)) =
                self.world.block_entities.get(&key)
            else {
                continue;
            };
            let items: Vec<Option<(String, i32)>> = entity
                .items
                .iter()
                .map(|stack| {
                    stack
                        .as_ref()
                        .map(|stack| (stack.item.to_string(), stack.count as i32))
                        .filter(|(item, count)| *count > 0 && item != "minecraft:air")
                })
                .collect();
            bridge.sync_menu_items(pos, &block, &items);
        }
        let mut adapter = HostBridgeAdapter(self);
        bridge.tick_block_entities(&mut adapter);
    }

    /// The position and block of every open Fabric menu (`Game::fabric_menu`).
    fn fabric_menu_items(&self) -> Vec<([i32; 3], String)> {
        self.fabric_menu
            .as_ref()
            .map(|(key, block, ..)| (([key.0, key.1, key.2]), block.clone()))
            .into_iter()
            .collect()
    }

    /// The block entity's own saved state, for the host to store with the
    /// block, and a load back into a fresh entity when the chunk comes in.
    pub(super) fn fabric_save_block_entity(
        &mut self,
        pos: [i32; 3],
    ) -> Option<Vec<(String, hfc_runtime::value_io_builtins::StoredValue)>> {
        let block = self
            .pack
            .assets
            .registry
            .state(self.world.block(pos[0], pos[1], pos[2]))
            .map(|state| state.name.to_string())?;
        let bridge = started_fabric_bridge()?;
        let mut bridge = bridge.lock().ok()?;
        bridge.save_block_entity(pos, &block)
    }

    pub(super) fn fabric_load_block_entity(
        &mut self,
        pos: [i32; 3],
        values: Vec<(String, hfc_runtime::value_io_builtins::StoredValue)>,
    ) {
        let Some(block) = self
            .pack
            .assets
            .registry
            .state(self.world.block(pos[0], pos[1], pos[2]))
            .map(|state| state.name.to_string())
        else {
            return;
        };
        let Some(bridge) = started_fabric_bridge() else {
            return;
        };
        let Ok(mut bridge) = bridge.lock() else {
            return;
        };
        if !bridge.is_registered_block(&block) {
            return;
        }
        let mut adapter = HostBridgeAdapter(self);
        bridge.load_block_entity(&adapter, pos, &block, values);
    }

    pub(super) fn fabric_tooltip(&self, stack: &ItemStack, lines: &mut Vec<(Text, u32)>) {
        let Ok(mut bridge) = get_fabric_bridge().lock() else {
            return;
        };
        let mut raw_lines: Vec<(String, u32)> =
            lines.iter().map(|(t, c)| (t.plain(), *c)).collect();
        bridge.item_tooltip(&stack.item, stack.count, &mut raw_lines);
        if raw_lines.len() != lines.len()
            || raw_lines
                .iter()
                .zip(lines.iter())
                .any(|((s, c), (t, tc))| s != &t.plain() || c != tc)
        {
            *lines = raw_lines
                .into_iter()
                .map(|(s, c)| (Text::legacy(&s), c))
                .collect();
        }
    }

    pub(super) fn fabric_tooltip_color(
        &self,
        stack: Option<&ItemStack>,
        background: &mut u32,
        border_top: &mut u32,
        border_bottom: &mut u32,
    ) {
        let Ok(mut bridge) = get_fabric_bridge().lock() else {
            return;
        };
        let rarity = stack.map(|stack| self.stack_rarity(stack));
        let item_id = stack.map(|s| &*s.item);
        bridge.item_tooltip_color(item_id, rarity, background, border_top, border_bottom);
    }

    /// The item's rarity as the host's item data reports it, the value
    /// `ItemStack.getRarity()` answers to mods.
    fn stack_rarity(&self, stack: &ItemStack) -> &str {
        let default = self
            .stations()
            .enchantments
            .props(&stack.item)
            .map_or("common", |props| props.rarity.as_str());
        horizon_client::item_tooltip::rarity(stack, default)
    }

    /// The mods' full tooltip: their real `ClientTooltipComponent` list
    /// (gathered, with a centred title) plus the `POSTEXT` frame, as draws
    /// relative to the tooltip's text origin. `None` when no Iceberg tooltip
    /// event has a listener.
    pub(super) fn fabric_tooltip_render(
        &self,
        stack: &ItemStack,
        lines: &[(Text, u32)],
        screen_width: i32,
        screen_height: i32,
    ) -> Option<hfc_runtime::bridge::TooltipRender> {
        let Ok(mut bridge) = get_fabric_bridge().lock() else {
            return None;
        };
        let rarity = self.stack_rarity(stack);
        let max_damage = self
            .items()
            .get(&stack.item)
            .map_or(0, |info| info.max_damage as i32);
        let raw_lines: Vec<(String, u32)> = lines
            .iter()
            .map(|(text, color)| (text.plain(), *color))
            .collect();
        bridge.item_tooltip_render(
            &stack.item,
            Some(rarity),
            max_damage,
            &raw_lines,
            screen_width,
            screen_height,
        )
    }

    /// `RenderTooltipEvents.POSTEXT`: let Iceberg-family mods draw a tooltip
    /// frame into a display list the host replays after the tooltip text.
    pub(super) fn fabric_tooltip_frame(
        &self,
        stack: &ItemStack,
        lines: &[(Text, u32)],
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        screen_width: i32,
        screen_height: i32,
    ) -> Vec<hfc_runtime::minecraft::GuiDraw> {
        let Ok(mut bridge) = get_fabric_bridge().lock() else {
            return Vec::new();
        };
        let rarity = self.stack_rarity(stack);
        let max_damage = self
            .items()
            .get(&stack.item)
            .map_or(0, |info| info.max_damage as i32);
        let raw_lines: Vec<(String, u32)> = lines
            .iter()
            .map(|(text, color)| (text.plain(), *color))
            .collect();
        bridge.item_tooltip_frame(
            &stack.item,
            Some(rarity),
            max_damage,
            &raw_lines,
            screen_width,
            screen_height,
            x,
            y,
            width,
            height,
        )
    }
}

impl super::GameLevel<'_> {
    /// The state a Fabric block is placed in, from its real
    /// `getStateForPlacement` run over this click. Asked for every block
    /// placed (`before_state_for_placement`); `answer` stays empty for a block
    /// no Fabric mod registered or one that keeps vanilla's placement. A mod
    /// that returns null refuses the placement; a state that has no Horizon
    /// state is logged and the block is placed as the host would place it.
    pub(super) fn fabric_state_for_placement(
        &self,
        state: BlockStateId,
        context: &PlacementContext,
        answer: &mut Option<Option<BlockStateId>>,
    ) {
        use hfc_runtime::bridge::Placement;
        if answer.is_some() {
            return;
        }
        let Some(block) = self.native_state_block(state) else {
            return;
        };
        let Some(bridge) = started_fabric_bridge() else {
            return;
        };
        // The local player's feet, from its box; the click point where the
        // level has no player box (a placement for a guest).
        let player_pos = self.player_box.map_or(
            [context.hit.x, context.hit.y, context.hit.z],
            |(min, max)| [(min[0] + max[0]) / 2.0, min[1], (min[2] + max[2]) / 2.0],
        );
        let request = hfc_runtime::bridge::PlacementRequest {
            block: block.to_owned(),
            pos: [context.pos.x, context.pos.y, context.pos.z],
            hit_pos: [context.hit_pos.x, context.hit_pos.y, context.hit_pos.z],
            face: context.face.normal(),
            hit: [context.hit.x, context.hit.y, context.hit.z],
            inside: context.inside,
            yaw: context.yaw,
            pitch: context.pitch,
            player_pos,
            sneaking: context.sneaking,
            // The placement context does not say which hand placed it.
            off_hand: false,
            item: context.item.clone(),
            replace_clicked: context.replace_clicked,
        };
        let Ok(mut bridge) = bridge.lock() else {
            return;
        };
        match bridge.state_for_placement(&PlacementWorld(self), &request) {
            Placement::NotOverridden => {}
            Placement::Refused => *answer = Some(None),
            Placement::State(properties) => match placed_state(self.registry, state, &properties) {
                Some(placed) => *answer = Some(Some(placed)),
                None => log::warn!(
                    "Fabric: {block} chose {properties:?} when placed, which is none of its states"
                ),
            },
            Placement::Failed(error) => {
                log::warn!("Fabric: {block}'s getStateForPlacement failed: {error}");
            }
        }
    }
}

/// What a Fabric block's own world reads while it is placed: the host's real
/// blocks. It only reads.
struct PlacementWorld<'a, 'b>(&'a super::GameLevel<'b>);

impl hfc_runtime::bridge::HostBridge for PlacementWorld<'_, '_> {
    fn block(&self, pos: [i32; 3]) -> String {
        let state = self.0.world.block(pos[0], pos[1], pos[2]);
        self.0.facts.get(state).name.to_string()
    }
    fn block_property(&self, pos: [i32; 3], key: &str) -> Option<String> {
        let state = self.0.world.block(pos[0], pos[1], pos[2]);
        self.0.registry.state(state)?.properties.get(key).cloned()
    }
    fn set_block_property(&mut self, _: [i32; 3], _: &str, _: &str) -> bool {
        false
    }
    fn give(&mut self, _: &str, _: u32) -> Result<(), String> {
        Err("placement does not give items".into())
    }
    fn block_properties(&self, pos: [i32; 3]) -> Vec<(String, String)> {
        let state = self.0.world.block(pos[0], pos[1], pos[2]);
        self.0
            .registry
            .state(state)
            .map(|s| {
                s.properties
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The state of `state`'s block whose properties are `properties` (every
/// property the block has, as `getStateForPlacement` leaves them); none when
/// the block has no such state.
fn placed_state(
    registry: &horizon_world::registry::BlockRegistry,
    state: BlockStateId,
    properties: &std::collections::BTreeMap<String, String>,
) -> Option<BlockStateId> {
    let mut wanted = registry.state(state)?.clone();
    for (name, value) in properties {
        // A property the block lacks is a state it does not have.
        wanted.properties.get(name)?;
        wanted
            .properties
            .insert(name.as_str().into(), value.as_str().into());
    }
    registry.resolve(&wanted).ok()
}

#[cfg(test)]
mod tests {
    use super::placed_state;
    use horizon_world::registry::{BlockRegistry, BlockStateId};
    use std::collections::BTreeMap;

    /// A mod block with a `facing` property is placed in the state the mod
    /// chose, not its default; a value or property the block lacks is none.
    #[test]
    fn a_chosen_placement_state_is_found_in_the_registry() {
        let facts = br#"{"defaults":{"minecraft:air":0,"test:cabinet":1},"states":[
            ["air",true,false,"","minecraft:air"],
            ["north",false,false,"",{"id":"test:cabinet","properties":{"facing":"north"}}],
            ["south",false,false,"",{"id":"test:cabinet","properties":{"facing":"south"}}],
            ["east",false,false,"",{"id":"test:cabinet","properties":{"facing":"east"}}]
        ]}"#;
        let registry = BlockRegistry::from_facts_json(&facts[..]).unwrap();
        let default = BlockStateId(1);
        let want = |k: &str, v: &str| BTreeMap::from([(k.to_owned(), v.to_owned())]);
        assert_eq!(
            placed_state(&registry, default, &want("facing", "east")),
            Some(BlockStateId(3))
        );
        assert_eq!(
            placed_state(&registry, default, &want("facing", "south")),
            Some(BlockStateId(2))
        );
        assert_eq!(
            placed_state(&registry, default, &want("facing", "up")),
            None
        );
        assert_eq!(placed_state(&registry, default, &want("half", "top")), None);
    }
}
