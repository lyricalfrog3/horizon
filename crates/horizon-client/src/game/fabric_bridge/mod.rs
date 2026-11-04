//! Bridge connecting Horizon's Game to the dynamic Fabric compatibility layer.
//!
//! `mod.rs` owns the runtime singleton and mod loading; the submodules hold the
//! GUI replay, menu widgets, texture upload and key numbering.

use super::Game;
use horizon_client::items::ItemStack;
use horizon_client::text::Text;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

mod gui;
mod keys;
mod menu;
mod textures;

pub(super) use gui::{
    fabric_screen_char, fabric_screen_close_current, fabric_screen_draws, fabric_screen_key,
    fabric_screen_mouse, fabric_screen_mouse_moved, fabric_screen_open, fabric_screen_scroll,
    replay_fabric_draws,
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

fn get_fabric_bridge() -> &'static Mutex<hfc_runtime::bridge::FabricBridge> {
    static BRIDGE: OnceLock<Mutex<hfc_runtime::bridge::FabricBridge>> = OnceLock::new();
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
        canceled: &mut bool,
    ) {
        if *canceled {
            return;
        }

        let event = hfc_runtime::bridge::RightClickBlockEvent { pos, face };
        if let Ok(mut bridge) = get_fabric_bridge().lock() {
            let mut adapter = HostBridgeAdapter(self);
            if bridge.right_click_block(&mut adapter, &event) == hfc_runtime::bridge::Flow::Cancel {
                *canceled = true;
            }
        }
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
        let item_id = stack.map(|s| &*s.item);
        bridge.item_tooltip_color(item_id, background, border_top, border_bottom);
    }
}
