//! Widgets a mod's mixin added to a native menu (title, pause): where the host
//! places them, drawing them and routing clicks.

use super::super::Game;
use horizon_client::hud::HudBatch;

use super::get_fabric_bridge;
use super::gui::{replay_fabric_draws, translate_draw};
use hfc_runtime::minecraft::{MenuKind, MenuWidget};

/// Where a contributed widget sits on a native menu. Mod widgets are host-placed:
/// the pause menu appends icon-sized ones to its icon row and wide ones under
/// the button grid.
pub(super) fn menu_widget_origin(
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

/// Widgets a mod's mixin added to a host menu.
pub(in super::super) fn menu_widgets(kind: MenuKind, width: i32, height: i32) -> Vec<MenuWidget> {
    match get_fabric_bridge().lock() {
        Ok(mut bridge) => bridge.prepare_menu_contributions(kind, width, height),
        Err(_) => Vec::new(),
    }
}

/// Draw a host menu's contributed widgets with its own cursor position.
pub(in super::super) fn draw_menu_widgets(
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

pub(super) fn draw_menu_widgets_onto(
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
pub(in super::super) fn menu_widget_press(
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

impl Game<'_> {
    pub(in super::super) fn draw_fabric_pause_widgets(&mut self, batch: &mut HudBatch) {
        let rows = self.pause_extra_rows();
        draw_menu_widgets(batch, &self.gui.atlas, self.cursor, MenuKind::Pause, rows);
    }

    /// A click inside a contributed pause-menu widget; false when the pointer
    /// is not over one, so native pause buttons still receive their clicks.
    pub(in super::super) fn fabric_pause_widget_press(&mut self) -> bool {
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
}
