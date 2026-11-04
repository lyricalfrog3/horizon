//! winit key and button numbering to the SDL values 26.3's input classes use.

use winit::keyboard::KeyCode;

/// The SDL scancode a Fabric `KeyEvent` carries in `key`. 26.3's
/// `InputWithModifiers` tests scancodes (`isEscape()` is 41), not GLFW key
/// numbers.
pub(super) fn sdl_scancode(key: KeyCode) -> i32 {
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
pub(super) fn sdl_keycode(key: KeyCode) -> i32 {
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
pub(super) fn sdl_button(button: winit::event::MouseButton) -> i32 {
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
