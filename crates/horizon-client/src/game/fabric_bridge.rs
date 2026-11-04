//! Bridge connecting Horizon's Game to the dynamic Fabric compatibility layer.

use super::{Game, Screen};
use horizon_client::hud::{HudBatch, rgba};
use horizon_client::items::ItemStack;
use horizon_client::text::Text;
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use winit::keyboard::KeyCode;

use hfc_runtime::minecraft::{GuiDraw, MenuKind, MenuWidget};

/// Atlas rects of the mod textures already uploaded into the HUD atlas this
/// session, by identifier. A mod's registered icon has no slot in the packed
/// atlas, so its PNG is decoded and placed in the run-time region once.
fn fabric_texture_rects() -> &'static Mutex<HashMap<String, horizon_client::hud::Rect>> {
    static RECTS: OnceLock<Mutex<HashMap<String, horizon_client::hud::Rect>>> = OnceLock::new();
    RECTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Identifiers with no image to draw (no jar provides one, or it does not
/// decode); they are not looked up again every frame.
fn fabric_texture_failed() -> &'static Mutex<BTreeSet<String>> {
    static FAILED: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
    FAILED.get_or_init(|| Mutex::new(BTreeSet::new()))
}

/// The font epoch the cached atlas rects were placed under. The run-time
/// region is shared with rasterized glyphs and restarts from its beginning
/// whenever it fills (`Font::begin_frame`), which overwrites every image placed
/// there; the rects must be forgotten and the images placed again.
fn fabric_texture_epoch() -> &'static std::sync::atomic::AtomicU64 {
    static EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);
    &EPOCH
}

/// Upload the images a display list blits that the HUD atlas does not hold:
/// textures a mod registered at run time and GUI sprites or textures it ships
/// in its own jar. Each is decoded and placed in the atlas's run-time region
/// and found by rect on later frames, until the region restarts. Host-menu
/// widgets and Fabric screens share this, since both replay recorded blits.
fn upload_fabric_textures(
    batch: &mut HudBatch,
    atlas: &horizon_client::hud::HudAtlas,
    draws: &[GuiDraw],
) {
    let epoch = atlas.font.epoch();
    if fabric_texture_epoch().swap(epoch, std::sync::atomic::Ordering::Relaxed) != epoch
        && let Ok(mut rects) = fabric_texture_rects().lock()
    {
        rects.clear();
    }
    let mut wanted: Vec<&str> = Vec::new();
    for draw in draws {
        let GuiDraw::Blit { texture, .. } = draw else {
            continue;
        };
        let name = texture.strip_prefix("minecraft:").unwrap_or(texture);
        if atlas.sprites.contains_key(name)
            || atlas.sprites.contains_key(&format!("gui/sprites/{name}"))
            || wanted.contains(&texture.as_str())
        {
            continue;
        }
        let known = fabric_texture_rects()
            .lock()
            .is_ok_and(|rects| rects.contains_key(texture))
            || fabric_texture_failed()
                .lock()
                .is_ok_and(|failed| failed.contains(texture));
        if !known {
            wanted.push(texture);
        }
    }
    if wanted.is_empty() {
        return;
    }
    let images: Vec<(String, Option<hfc_runtime::minecraft::PngImage>)> =
        match get_fabric_bridge().lock() {
            Ok(mut bridge) => wanted
                .iter()
                .map(|name| (name.to_string(), bridge.gui_texture(name)))
                .collect(),
            Err(_) => return,
        };
    let mut placed = Vec::new();
    let mut failed = Vec::new();
    for (name, image) in images {
        let decoded = image.and_then(|image| horizon_client::hud::decode_bytes(&image.png).ok());
        let Some((width, height, rgba)) = decoded else {
            failed.push(name);
            continue;
        };
        let (width, height, rgba) = fit_texture(width, height, rgba);
        // A full region is not a failure: it restarts at the next frame and
        // the image is placed again then.
        if let Some(rect) = atlas.font.place_image(width, height, rgba) {
            placed.push((name, rect));
        }
    }
    batch.patches.extend(atlas.font.take_patches());
    if let Ok(mut rects) = fabric_texture_rects().lock() {
        rects.extend(placed);
    }
    if let Ok(mut set) = fabric_texture_failed().lock() {
        set.extend(failed);
    }
}

/// Cap a mod icon at 64 px per side: the list draws it at 32 GUI pixels and
/// the run-time atlas region is shared with rasterized glyphs.
fn fit_texture(width: u32, height: u32, rgba: Vec<u8>) -> (u32, u32, Vec<u8>) {
    const MAX: u32 = 64;
    let longest = width.max(height);
    if longest <= MAX || width == 0 || height == 0 {
        return (width, height, rgba);
    }
    let scale = f64::from(MAX) / f64::from(longest);
    let new_width = ((f64::from(width) * scale).round() as u32).max(1);
    let new_height = ((f64::from(height) * scale).round() as u32).max(1);
    let mut out = Vec::with_capacity((new_width * new_height * 4) as usize);
    for y in 0..new_height {
        let source_y = (u64::from(y) * u64::from(height) / u64::from(new_height)) as u32;
        for x in 0..new_width {
            let source_x = (u64::from(x) * u64::from(width) / u64::from(new_width)) as u32;
            let at = ((source_y * width + source_x) * 4) as usize;
            out.extend_from_slice(&rgba[at..at + 4]);
        }
    }
    (new_width, new_height, out)
}

/// The letter shown on a texture placeholder. A mod's registered texture is
/// named after its icon; the last path segment is the most specific part.
fn texture_initial(name: &str) -> Option<String> {
    let stem = name.rsplit(['/', ':']).next()?;
    let stem = stem.strip_suffix("_icon").unwrap_or(stem);
    stem.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
}

/// Jars Options > Mods switched off: not loaded, but still listed so the
/// player can turn them back on.
fn switched_off_mods() -> &'static Mutex<Vec<hfc_runtime::bridge::JarSummary>> {
    static OFF: OnceLock<Mutex<Vec<hfc_runtime::bridge::JarSummary>>> = OnceLock::new();
    OFF.get_or_init(|| Mutex::new(Vec::new()))
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
        if let Err(e) = bridge.init() {
            log::warn!("Failed to initialize FabricBridge: {e}");
        }
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
    fn drop_item(&mut self, pos: [i32; 3], item: &str, count: u32) {
        let _ = self.0.native_give(item, count);
        self.0.pop_resource(pos, ItemStack::new(item, count));
    }
}

/// GuiGraphics colour is ARGB; the HUD vertex colour is ABGR.
fn abgr(color: u32) -> u32 {
    let [a, r, g, b] = color.to_be_bytes();
    rgba(r, g, b, a)
}

/// The SDL scancode a Fabric `KeyEvent` carries in `key`. 26.3's
/// `InputWithModifiers` tests scancodes (`isEscape()` is 41), not GLFW key
/// numbers.
fn sdl_scancode(key: KeyCode) -> i32 {
    match key {
        KeyCode::Escape => 41,
        KeyCode::Enter => 40,
        KeyCode::Tab => 43,
        KeyCode::Backspace => 42,
        KeyCode::Insert => 73,
        KeyCode::Delete => 76,
        KeyCode::ArrowRight => 79,
        KeyCode::ArrowLeft => 80,
        KeyCode::ArrowDown => 81,
        KeyCode::ArrowUp => 82,
        KeyCode::PageUp => 75,
        KeyCode::PageDown => 78,
        KeyCode::Home => 74,
        KeyCode::End => 77,
        KeyCode::CapsLock => 57,
        KeyCode::ScrollLock => 71,
        KeyCode::NumLock => 83,
        KeyCode::PrintScreen => 70,
        KeyCode::Pause => 72,
        KeyCode::F1 => 58,
        KeyCode::F2 => 59,
        KeyCode::F3 => 60,
        KeyCode::F4 => 61,
        KeyCode::F5 => 62,
        KeyCode::F6 => 63,
        KeyCode::F7 => 64,
        KeyCode::F8 => 65,
        KeyCode::F9 => 66,
        KeyCode::F10 => 67,
        KeyCode::F11 => 68,
        KeyCode::F12 => 69,
        KeyCode::ShiftLeft => 225,
        KeyCode::ControlLeft => 224,
        KeyCode::AltLeft => 226,
        KeyCode::SuperLeft => 227,
        KeyCode::ShiftRight => 229,
        KeyCode::ControlRight => 228,
        KeyCode::AltRight => 230,
        KeyCode::SuperRight => 231,
        KeyCode::Space => 44,
        KeyCode::Minus => 45,
        KeyCode::Equal => 46,
        KeyCode::BracketLeft => 47,
        KeyCode::BracketRight => 48,
        KeyCode::Backslash => 49,
        KeyCode::Semicolon => 51,
        KeyCode::Quote => 52,
        KeyCode::Comma => 54,
        KeyCode::Period => 55,
        KeyCode::Slash => 56,
        KeyCode::Backquote => 53,
        KeyCode::Digit1 => 30,
        KeyCode::Digit2 => 31,
        KeyCode::Digit3 => 32,
        KeyCode::Digit4 => 33,
        KeyCode::Digit5 => 34,
        KeyCode::Digit6 => 35,
        KeyCode::Digit7 => 36,
        KeyCode::Digit8 => 37,
        KeyCode::Digit9 => 38,
        KeyCode::Digit0 => 39,
        KeyCode::KeyA => 4,
        KeyCode::KeyB => 5,
        KeyCode::KeyC => 6,
        KeyCode::KeyD => 7,
        KeyCode::KeyE => 8,
        KeyCode::KeyF => 9,
        KeyCode::KeyG => 10,
        KeyCode::KeyH => 11,
        KeyCode::KeyI => 12,
        KeyCode::KeyJ => 13,
        KeyCode::KeyK => 14,
        KeyCode::KeyL => 15,
        KeyCode::KeyM => 16,
        KeyCode::KeyN => 17,
        KeyCode::KeyO => 18,
        KeyCode::KeyP => 19,
        KeyCode::KeyQ => 20,
        KeyCode::KeyR => 21,
        KeyCode::KeyS => 22,
        KeyCode::KeyT => 23,
        KeyCode::KeyU => 24,
        KeyCode::KeyV => 25,
        KeyCode::KeyW => 26,
        KeyCode::KeyX => 27,
        KeyCode::KeyY => 28,
        KeyCode::KeyZ => 29,
        _ => -1,
    }
}

/// The SDL keycode a Fabric `KeyEvent` carries in `keycode`. 26.3's
/// `InputWithModifiers.shortcutKey()` hands this to `Screen.keyPressed` and
/// `EditBox.keyPressed`, whose switches read Tab (9), Backspace (8), Delete
/// (127), Home/End and the arrow keycodes (not the scancodes in `key`).
fn sdl_keycode(key: KeyCode) -> i32 {
    match key {
        KeyCode::Backspace => 8,
        KeyCode::Tab => 9,
        KeyCode::Enter => 13,
        KeyCode::Escape => 27,
        KeyCode::Delete => 127,
        KeyCode::Insert => 1_073_741_897,
        KeyCode::Home => 1_073_741_898,
        KeyCode::PageUp => 1_073_741_899,
        KeyCode::End => 1_073_741_901,
        KeyCode::PageDown => 1_073_741_902,
        KeyCode::ArrowRight => 1_073_741_903,
        KeyCode::ArrowLeft => 1_073_741_904,
        KeyCode::ArrowDown => 1_073_741_905,
        KeyCode::ArrowUp => 1_073_741_906,
        _ => 0,
    }
}

/// The SDL mouse button number 26.3's `MouseButtonInfo` carries:
/// left is 1 (`AbstractWidget.isValidClickButton` accepts exactly that),
/// middle 2, right 3, back 4, forward 5. GLFW's left is 0, so passing
/// `glfw_button` here made every widget click fail the validity check even
/// though the screen reported the click as handled.
fn sdl_button(button: winit::event::MouseButton) -> i32 {
    use winit::event::MouseButton;
    match button {
        MouseButton::Left => 1,
        MouseButton::Middle => 2,
        MouseButton::Right => 3,
        MouseButton::Back => 4,
        MouseButton::Forward => 5,
        MouseButton::Other(button) => i32::from(button),
    }
}

/// Where a contributed widget sits on a native menu. Mod widgets are host-placed:
/// the pause menu appends icon-sized ones to its icon row and wide ones under
/// the button grid.
fn menu_widget_origin(
    kind: MenuKind,
    extra_rows: u8,
    width: f32,
    height: f32,
    index: usize,
    widget: &MenuWidget,
) -> (f32, f32, f32, f32) {
    match kind {
        MenuKind::Pause => {
            // The native grid's origin (`PauseScreen`'s `GridLayout`, 212 wide
            // and 166 high plus a row per server dialog/links/guest row), the
            // same one its buttons and icon row are placed from.
            let down = 24.0 * f32::from(extra_rows);
            let x0 = ((width - 212.0) * 0.5).floor();
            let y0 = ((height - (166.0 + down)) * 0.25).floor();
            if widget.width <= 24 && widget.height <= 24 {
                // The four native icons sit at 60, 84, 108 and 132.
                (
                    x0 + 156.0 + 24.0 * index as f32,
                    y0 + 98.0,
                    widget.width as f32,
                    widget.height as f32,
                )
            } else {
                (
                    x0 + 4.0,
                    y0 + 170.0 + down + 24.0 * index as f32,
                    widget.width as f32,
                    widget.height as f32,
                )
            }
        }
        MenuKind::Title => {
            let cx = (width / 2.0).floor();
            let y = (height / 4.0).floor() + 48.0;
            if widget.width <= 24 && widget.height <= 24 {
                (
                    cx - 34.0 + 72.0 + 24.0 * index as f32,
                    y + 72.0,
                    widget.width as f32,
                    widget.height as f32,
                )
            } else {
                (
                    cx - 102.0,
                    y + 96.0 + 24.0 * index as f32,
                    widget.width as f32,
                    widget.height as f32,
                )
            }
        }
        _ => (
            8.0,
            8.0 + 24.0 * index as f32,
            widget.width as f32,
            widget.height as f32,
        ),
    }
}

fn translate_draw(draw: &mut GuiDraw, dx: i32, dy: i32) {
    match draw {
        GuiDraw::Fill { x1, y1, x2, y2, .. } => {
            *x1 += dx;
            *y1 += dy;
            *x2 += dx;
            *y2 += dy;
        }
        GuiDraw::Text { x, y, .. } => {
            *x += dx;
            *y += dy;
        }
        GuiDraw::Blit { x, y, .. } => {
            *x += dx;
            *y += dy;
        }
        GuiDraw::Scissor { x1, y1, x2, y2 } => {
            *x1 += dx;
            *y1 += dy;
            *x2 += dx;
            *y2 += dy;
        }
        GuiDraw::ScissorOff => {}
    }
}

/// Replay a recorded `GuiGraphicsExtractor` display list through the
/// HUD batch. Fabric screens and host-menu contribution widgets share it.
pub(super) fn replay_fabric_draws(
    batch: &mut HudBatch,
    atlas: &horizon_client::hud::HudAtlas,
    draws: Vec<hfc_runtime::minecraft::GuiDraw>,
) {
    // Every caller replays through here, so the images the blits name (a mod's
    // icons and sprites) are in the atlas whichever screen opened them.
    upload_fabric_textures(batch, atlas, &draws);
    let mut scissors: Vec<(usize, (f32, f32, f32, f32))> = Vec::new();
    for draw in draws {
        match draw {
            hfc_runtime::minecraft::GuiDraw::Fill {
                x1,
                y1,
                x2,
                y2,
                color,
            } => {
                let (x, w) = (x1.min(x2) as f32, (x2 - x1).abs() as f32);
                let (y, h) = (y1.min(y2) as f32, (y2 - y1).abs() as f32);
                batch.fill(atlas, x, y, w, h, abgr(color as u32));
            }
            hfc_runtime::minecraft::GuiDraw::Text { text, x, y, color } => {
                if !text.is_empty() && text != "null" && !text.starts_with("[Object ") {
                    batch.text(atlas, &text, x as f32, y as f32, abgr(color as u32), true);
                }
            }
            hfc_runtime::minecraft::GuiDraw::Blit {
                texture,
                x,
                y,
                width,
                height,
            } => {
                let name = texture.strip_prefix("minecraft:").unwrap_or(&texture);
                // A widget identifier is `minecraft:widget/button`; the
                // HUD atlas stores the same vanilla sprite as
                // `gui/sprites/widget/button`. Try both names before giving
                // up, so Fabric screens get the real widget textures rather
                // than text floating over the world.
                let sprite = if atlas.sprites.contains_key(name) {
                    Some(name.to_string())
                } else if atlas.sprites.contains_key(&format!("gui/sprites/{name}")) {
                    Some(format!("gui/sprites/{name}"))
                } else {
                    None
                };
                if let Some(sprite) = sprite {
                    // `blitSprite` honours the sprite's `gui.scaling`: a
                    // 20 px button keeps its 3 px borders rather than
                    // squeezing the 200 px sprite.
                    horizon_client::ui::blit_sprite(
                        batch,
                        atlas,
                        &sprite,
                        x as f32,
                        y as f32,
                        width as f32,
                        height as f32,
                        u32::MAX,
                    );
                } else if let Ok(rects) = fabric_texture_rects().lock()
                    && let Some(rect) = rects.get(&texture).copied()
                {
                    // A mod's registered texture (its own icon, an image it
                    // read with `NativeImage.read` and registered through
                    // `TextureManager`): decoded and placed in the atlas's
                    // run-time region on the frame it first appeared.
                    batch.rect_uv(
                        x as f32,
                        y as f32,
                        width as f32,
                        height as f32,
                        rect,
                        u32::MAX,
                    );
                } else if name.contains("textures/gui/") {
                    // Full GUI backgrounds, separators and panel textures
                    // are not sprites in the HUD atlas. A dark translucent
                    // fill keeps a Fabric screen readable over the world
                    // until a GUI texture atlas can supply the exact image.
                    let color = if name.contains("background") {
                        rgba(0, 0, 0, 190)
                    } else if name.contains("separator") {
                        rgba(0, 0, 0, 130)
                    } else {
                        rgba(0, 0, 0, 90)
                    };
                    batch.fill(
                        atlas,
                        x as f32,
                        y as f32,
                        width as f32,
                        height as f32,
                        color,
                    );
                } else {
                    // A mod's own registered texture is not in Horizon's
                    // HUD atlas. Draw a deterministic tile instead of an
                    // empty cell, so a list of icons stays readable.
                    let hash = name.bytes().fold(2_166_136_261u32, |hash, byte| {
                        (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
                    });
                    let color = rgba(
                        64 + (hash & 0x7f) as u8,
                        64 + ((hash >> 8) & 0x7f) as u8,
                        64 + ((hash >> 16) & 0x7f) as u8,
                        220,
                    );
                    batch.fill(
                        atlas,
                        x as f32,
                        y as f32,
                        width as f32,
                        height as f32,
                        color,
                    );
                    if width >= 24
                        && height >= 24
                        && let Some(label) = texture_initial(name)
                    {
                        batch.text(
                            atlas,
                            &label,
                            x as f32 + 9.0,
                            y as f32 + 8.0,
                            rgba(255, 255, 255, 255),
                            true,
                        );
                    }
                }
            }
            hfc_runtime::minecraft::GuiDraw::Scissor { x1, y1, x2, y2 } => {
                scissors.push((
                    batch.vertices.len(),
                    (x1 as f32, y1 as f32, x2 as f32, y2 as f32),
                ));
            }
            hfc_runtime::minecraft::GuiDraw::ScissorOff => {
                if let Some((start, (x1, y1, x2, y2))) = scissors.pop() {
                    batch.clip_since(start, x1, y1, x2, y2);
                }
            }
        }
    }
    // A screen that leaves a scissor enabled still only paints inside the
    // last bounds it set.
    for (start, (x1, y1, x2, y2)) in scissors.into_iter().rev() {
        batch.clip_since(start, x1, y1, x2, y2);
    }
}

/// Widgets a mod's mixin added to a host menu.
pub(super) fn menu_widgets(kind: MenuKind, width: i32, height: i32) -> Vec<MenuWidget> {
    match get_fabric_bridge().lock() {
        Ok(mut bridge) => bridge.prepare_menu_contributions(kind, width, height),
        Err(_) => Vec::new(),
    }
}

/// Draw a host menu's contributed widgets with its own cursor position.
pub(super) fn draw_menu_widgets(
    batch: &mut HudBatch,
    atlas: &horizon_client::hud::HudAtlas,
    cursor: (f32, f32),
    kind: MenuKind,
    extra_rows: u8,
) {
    let has_injectors = match get_fabric_bridge().lock() {
        Ok(bridge) => bridge.has_menu_injectors(kind),
        Err(_) => false,
    };
    if !has_injectors {
        return;
    }
    let widgets = menu_widgets(kind, batch.width as i32, batch.height as i32);
    draw_menu_widgets_onto(batch, atlas, cursor, kind, extra_rows, &widgets);
}

fn draw_menu_widgets_onto(
    batch: &mut HudBatch,
    atlas: &horizon_client::hud::HudAtlas,
    cursor: (f32, f32),
    kind: MenuKind,
    extra_rows: u8,
    widgets: &[MenuWidget],
) {
    let (width, height) = (batch.width as i32, batch.height as i32);
    for (index, widget) in widgets.iter().enumerate() {
        let (origin_x, origin_y, _, _) =
            menu_widget_origin(kind, extra_rows, batch.width, batch.height, index, widget);
        // The widget's own extraction draws at its runtime x/y; the host may
        // place it elsewhere (a synthesized layout leaves x/y at 0), so shift
        // the draws by the difference.
        let offset_x = (origin_x - widget.x as f32) as i32;
        let offset_y = (origin_y - widget.y as f32) as i32;
        let mut draws = match get_fabric_bridge().lock() {
            Ok(mut bridge) => bridge.menu_widget_draw(
                kind,
                width,
                height,
                index,
                cursor.0 as i32 - origin_x as i32 + widget.x,
                cursor.1 as i32 - origin_y as i32 + widget.y,
            ),
            Err(_) => Vec::new(),
        };
        if draws.is_empty() {
            continue;
        }
        for draw in &mut draws {
            translate_draw(draw, offset_x, offset_y);
        }
        replay_fabric_draws(batch, atlas, draws);
    }
}

/// A click inside a host menu's contributed widget. False when the pointer is
/// outside every widget, so the native menu receives the click.
pub(super) fn menu_widget_press(
    kind: MenuKind,
    extra_rows: u8,
    width: f32,
    height: f32,
    cursor: (f32, f32),
    button: i32,
    pressed: bool,
) -> bool {
    let widgets = menu_widgets(kind, width as i32, height as i32);
    for (index, widget) in widgets.iter().enumerate() {
        let (origin_x, origin_y, widget_width, widget_height) =
            menu_widget_origin(kind, extra_rows, width, height, index, widget);
        let (x, y) = (f64::from(cursor.0), f64::from(cursor.1));
        if x < f64::from(origin_x)
            || y < f64::from(origin_y)
            || x >= f64::from(origin_x + widget_width)
            || y >= f64::from(origin_y + widget_height)
        {
            continue;
        }
        if let Ok(mut bridge) = get_fabric_bridge().lock() {
            let _ = bridge.menu_widget_click(
                kind,
                width as i32,
                height as i32,
                index,
                f64::from(widget.x) + (x - f64::from(origin_x)),
                f64::from(widget.y) + (y - f64::from(origin_y)),
                button,
                pressed,
            );
        }
        return true;
    }
    false
}

/// Whether a Fabric mod installed a screen while a host menu is open.
pub(super) fn fabric_screen_open() -> bool {
    get_fabric_bridge()
        .lock()
        .map(|bridge| bridge.has_screen())
        .unwrap_or(false)
}

/// The open Fabric screen's display list, for a host menu that draws it.
pub(super) fn fabric_screen_draws(width: i32, height: i32) -> Vec<GuiDraw> {
    match get_fabric_bridge().lock() {
        Ok(mut bridge) => bridge
            .render_screen(width, height, 0.0)
            .ok()
            .flatten()
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Close the Fabric screen over a host menu (`Escape`/Done).
pub(super) fn fabric_screen_close_current() {
    if let Ok(mut bridge) = get_fabric_bridge().lock() {
        bridge.close_screen();
    }
}

/// Key/char/mouse dispatch while a Fabric screen is over a host menu.
pub(super) fn fabric_screen_key(key: KeyCode, pressed: bool) {
    if let Ok(mut bridge) = get_fabric_bridge().lock() {
        bridge.key_event(sdl_scancode(key), sdl_keycode(key), 0, pressed);
    }
}

pub(super) fn fabric_screen_char(c: char) {
    if let Ok(mut bridge) = get_fabric_bridge().lock() {
        bridge.char_event(c as i32);
    }
}

pub(super) fn fabric_screen_mouse_moved(x: f32, y: f32) {
    if let Ok(mut bridge) = get_fabric_bridge().lock() {
        bridge.mouse_moved(f64::from(x), f64::from(y));
    }
}

pub(super) fn fabric_screen_mouse(
    button: winit::event::MouseButton,
    x: f32,
    y: f32,
    pressed: bool,
) {
    if let Ok(mut bridge) = get_fabric_bridge().lock() {
        bridge.mouse_button(f64::from(x), f64::from(y), sdl_button(button), pressed);
    }
}

pub(super) fn fabric_screen_scroll(x: f32, y: f32, dx: f32, dy: f32) {
    if let Ok(mut bridge) = get_fabric_bridge().lock() {
        bridge.mouse_scrolled(f64::from(x), f64::from(y), f64::from(dx), f64::from(dy));
    }
}

impl Game<'_> {
    /// Run Fabric's client tick and follow the screen a mod installed. A screen
    /// opening over gameplay takes the same input as the host's own screens.
    pub(super) fn fabric_screen_tick(&mut self) {
        if let Ok(mut bridge) = get_fabric_bridge().lock() {
            bridge.client_tick();
        }
        self.fabric_follow_screen();
    }

    /// Follow the screen a mod installed, without running a client tick: a
    /// click on a widget a mod added to a host menu opens it immediately, even
    /// when the paused game is not ticking.
    pub(super) fn fabric_follow_screen(&mut self) {
        let (open, screen) = if let Ok(bridge) = get_fabric_bridge().lock() {
            (bridge.has_screen(), bridge.current_screen())
        } else {
            (false, None)
        };
        if open && screen.is_some() {
            match self.screen {
                Screen::Playing => {
                    self.screen = Screen::Fabric;
                    self.capture(false);
                }
                // A mod screen opened from the pause/options screen returns to
                // that native screen when it closes.
                Screen::Paused | Screen::Options => {
                    self.fabric_return = Some(self.screen);
                    self.screen = Screen::Fabric;
                    self.capture(false);
                }
                _ => {}
            }
        } else if !open && self.screen == Screen::Fabric {
            self.fabric_restore_screen();
        }
    }

    fn fabric_restore_screen(&mut self) {
        let previous = self.fabric_return.take().unwrap_or(Screen::Playing);
        self.screen = previous;
        self.capture(matches!(previous, Screen::Playing));
    }

    /// Open the screen a Fabric key mapping installed (the host's binding for
    /// the mapping's translation key). Returns whether a mapping was found.
    pub(super) fn fabric_press_key_mapping(&mut self, name: &str) -> bool {
        let pressed = match get_fabric_bridge().lock() {
            Ok(mut bridge) => bridge.press_key_mapping(name),
            Err(_) => false,
        };
        if pressed {
            self.fabric_screen_tick();
        }
        pressed
    }

    pub(super) fn fabric_screen_key(&mut self, key: KeyCode, pressed: bool) {
        let modifiers = super::glfw_modifiers(self.modifiers) as i32;
        if let Ok(mut bridge) = get_fabric_bridge().lock() {
            bridge.key_event(sdl_scancode(key), sdl_keycode(key), modifiers, pressed);
        }
        self.fabric_screen_tick();
    }

    pub(super) fn fabric_screen_char(&mut self, c: char) {
        if let Ok(mut bridge) = get_fabric_bridge().lock() {
            bridge.char_event(c as i32);
        }
    }

    pub(super) fn fabric_screen_mouse_moved(&mut self) {
        if let Ok(mut bridge) = get_fabric_bridge().lock() {
            bridge.mouse_moved(f64::from(self.cursor.0), f64::from(self.cursor.1));
        }
    }

    pub(super) fn fabric_screen_mouse(&mut self, button: winit::event::MouseButton, pressed: bool) {
        if let Ok(mut bridge) = get_fabric_bridge().lock() {
            bridge.mouse_button(
                f64::from(self.cursor.0),
                f64::from(self.cursor.1),
                sdl_button(button),
                pressed,
            );
        }
        self.fabric_screen_tick();
    }

    pub(super) fn fabric_screen_scroll(&mut self, dx: f64, dy: f64) {
        if let Ok(mut bridge) = get_fabric_bridge().lock() {
            bridge.mouse_scrolled(f64::from(self.cursor.0), f64::from(self.cursor.1), dx, dy);
        }
    }

    pub(super) fn fabric_screen_close(&mut self) {
        if let Ok(mut bridge) = get_fabric_bridge().lock() {
            bridge.close_screen();
            if bridge.has_screen() {
                return;
            }
        }
        self.fabric_restore_screen();
    }

    /// Replay the open Fabric screen's `GuiGraphicsExtractor` display list.
    /// Widget sprites come from the HUD atlas, GUI backgrounds get a dark
    /// fallback until they are packed, and scissors clip the recorded quads.
    pub(super) fn draw_fabric_screen(&mut self, batch: &mut HudBatch) {
        let draws = match get_fabric_bridge().lock() {
            Ok(mut bridge) => bridge
                .render_screen(batch.width as i32, batch.height as i32, 0.0)
                .ok()
                .flatten()
                .unwrap_or_default(),
            Err(_) => return,
        };
        replay_fabric_draws(batch, &self.gui.atlas, draws);
    }

    pub(super) fn draw_fabric_pause_widgets(&mut self, batch: &mut HudBatch) {
        let rows = self.pause_extra_rows();
        draw_menu_widgets(batch, &self.gui.atlas, self.cursor, MenuKind::Pause, rows);
    }

    /// A click inside a contributed pause-menu widget; false when the pointer
    /// is not over one, so native pause buttons still receive their clicks.
    pub(super) fn fabric_pause_widget_press(&mut self) -> bool {
        let Some(renderer) = &self.renderer else {
            return false;
        };
        let batch = HudBatch::new([renderer.config.width, renderer.config.height]);
        let handled = menu_widget_press(
            MenuKind::Pause,
            self.pause_extra_rows(),
            batch.width,
            batch.height,
            self.cursor,
            1,
            true,
        );
        if handled {
            self.fabric_follow_screen();
        }
        handled
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
