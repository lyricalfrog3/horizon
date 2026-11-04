//! Mod textures and sprites in the HUD atlas: decode once, place in the run-time
//! region, forget the rects when the glyph region restarts.

use horizon_client::hud::HudBatch;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, OnceLock};

use super::get_fabric_bridge;
use hfc_runtime::minecraft::GuiDraw;

/// Atlas rects of the mod textures already uploaded into the HUD atlas this
/// session, by identifier. A mod's registered icon has no slot in the packed
/// atlas, so its PNG is decoded and placed in the run-time region once.
pub(super) fn fabric_texture_rects() -> &'static Mutex<HashMap<String, horizon_client::hud::Rect>> {
    static RECTS: OnceLock<Mutex<HashMap<String, horizon_client::hud::Rect>>> = OnceLock::new();
    RECTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Identifiers with no image to draw (no jar provides one, or it does not
/// decode); they are not looked up again every frame.
pub(super) fn fabric_texture_failed() -> &'static Mutex<BTreeSet<String>> {
    static FAILED: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
    FAILED.get_or_init(|| Mutex::new(BTreeSet::new()))
}

/// The font epoch the cached atlas rects were placed under. The run-time
/// region is shared with rasterized glyphs and restarts from its beginning
/// whenever it fills (`Font::begin_frame`), which overwrites every image placed
/// there; the rects must be forgotten and the images placed again.
pub(super) fn fabric_texture_epoch() -> &'static std::sync::atomic::AtomicU64 {
    static EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);
    &EPOCH
}

/// Upload the images a display list blits that the HUD atlas does not hold:
/// textures a mod registered at run time and GUI sprites or textures it ships
/// in its own jar. Each is decoded and placed in the atlas's run-time region
/// and found by rect on later frames, until the region restarts. Host-menu
/// widgets and Fabric screens share this, since both replay recorded blits.
pub(super) fn upload_fabric_textures(
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
pub(super) fn fit_texture(width: u32, height: u32, rgba: Vec<u8>) -> (u32, u32, Vec<u8>) {
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
pub(super) fn texture_initial(name: &str) -> Option<String> {
    let stem = name.rsplit(['/', ':']).next()?;
    let stem = stem.strip_suffix("_icon").unwrap_or(stem);
    stem.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
}
