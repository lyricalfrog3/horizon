//! Replaying a Fabric screen's recorded display list, and following and
//! driving the screen a mod installed.

use super::super::{Game, Screen};
use horizon_client::hud::{HudBatch, rgba};
use winit::keyboard::KeyCode;

use super::get_fabric_bridge;
use super::keys::{sdl_button, sdl_keycode, sdl_scancode};
use super::textures::{fabric_texture_rects, texture_initial, upload_fabric_textures};
use hfc_runtime::minecraft::GuiDraw;

/// GuiGraphics colour is ARGB; the HUD vertex colour is ABGR.
pub(super) fn abgr(color: u32) -> u32 {
    let [a, r, g, b] = color.to_be_bytes();
    rgba(r, g, b, a)
}

pub(super) fn translate_draw(draw: &mut GuiDraw, dx: i32, dy: i32) {
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
pub(in super::super) fn replay_fabric_draws(
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

/// Whether a Fabric mod installed a screen while a host menu is open.
pub(in super::super) fn fabric_screen_open() -> bool {
    get_fabric_bridge()
        .lock()
        .map(|bridge| bridge.has_screen())
        .unwrap_or(false)
}

/// The open Fabric screen's display list, for a host menu that draws it.
pub(in super::super) fn fabric_screen_draws(width: i32, height: i32) -> Vec<GuiDraw> {
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
pub(in super::super) fn fabric_screen_close_current() {
    if let Ok(mut bridge) = get_fabric_bridge().lock() {
        bridge.close_screen();
    }
}

/// Key/char/mouse dispatch while a Fabric screen is over a host menu.
pub(in super::super) fn fabric_screen_key(key: KeyCode, pressed: bool) {
    if let Ok(mut bridge) = get_fabric_bridge().lock() {
        bridge.key_event(sdl_scancode(key), sdl_keycode(key), 0, pressed);
    }
}

pub(in super::super) fn fabric_screen_char(c: char) {
    if let Ok(mut bridge) = get_fabric_bridge().lock() {
        bridge.char_event(c as i32);
    }
}

pub(in super::super) fn fabric_screen_mouse_moved(x: f32, y: f32) {
    if let Ok(mut bridge) = get_fabric_bridge().lock() {
        bridge.mouse_moved(f64::from(x), f64::from(y));
    }
}

pub(in super::super) fn fabric_screen_mouse(
    button: winit::event::MouseButton,
    x: f32,
    y: f32,
    pressed: bool,
) {
    if let Ok(mut bridge) = get_fabric_bridge().lock() {
        bridge.mouse_button(f64::from(x), f64::from(y), sdl_button(button), pressed);
    }
}

pub(in super::super) fn fabric_screen_scroll(x: f32, y: f32, dx: f32, dy: f32) {
    if let Ok(mut bridge) = get_fabric_bridge().lock() {
        bridge.mouse_scrolled(f64::from(x), f64::from(y), f64::from(dx), f64::from(dy));
    }
}

impl Game<'_> {
    /// Run Fabric's client tick and follow the screen a mod installed. A screen
    /// opening over gameplay takes the same input as the host's own screens.
    pub(in super::super) fn fabric_screen_tick(&mut self) {
        if let Ok(mut bridge) = get_fabric_bridge().lock() {
            bridge.client_tick();
        }
        self.fabric_follow_screen();
    }

    /// Follow the screen a mod installed, without running a client tick: a
    /// click on a widget a mod added to a host menu opens it immediately, even
    /// when the paused game is not ticking.
    pub(in super::super) fn fabric_follow_screen(&mut self) {
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
    pub(in super::super) fn fabric_press_key_mapping(&mut self, name: &str) -> bool {
        let pressed = match get_fabric_bridge().lock() {
            Ok(mut bridge) => bridge.press_key_mapping(name),
            Err(_) => false,
        };
        if pressed {
            self.fabric_screen_tick();
        }
        pressed
    }

    pub(in super::super) fn fabric_screen_key(&mut self, key: KeyCode, pressed: bool) {
        let modifiers = super::super::glfw_modifiers(self.modifiers) as i32;
        if let Ok(mut bridge) = get_fabric_bridge().lock() {
            bridge.key_event(sdl_scancode(key), sdl_keycode(key), modifiers, pressed);
        }
        self.fabric_screen_tick();
    }

    pub(in super::super) fn fabric_screen_char(&mut self, c: char) {
        if let Ok(mut bridge) = get_fabric_bridge().lock() {
            bridge.char_event(c as i32);
        }
    }

    pub(in super::super) fn fabric_screen_mouse_moved(&mut self) {
        if let Ok(mut bridge) = get_fabric_bridge().lock() {
            bridge.mouse_moved(f64::from(self.cursor.0), f64::from(self.cursor.1));
        }
    }

    pub(in super::super) fn fabric_screen_mouse(
        &mut self,
        button: winit::event::MouseButton,
        pressed: bool,
    ) {
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

    pub(in super::super) fn fabric_screen_scroll(&mut self, dx: f64, dy: f64) {
        if let Ok(mut bridge) = get_fabric_bridge().lock() {
            bridge.mouse_scrolled(f64::from(self.cursor.0), f64::from(self.cursor.1), dx, dy);
        }
    }

    pub(in super::super) fn fabric_screen_close(&mut self) {
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
    pub(in super::super) fn draw_fabric_screen(&mut self, batch: &mut HudBatch) {
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
}
