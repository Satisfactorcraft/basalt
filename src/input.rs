//! Keyboard and mouse state, queried by name from scripts.

use std::collections::HashSet;

use winit::event::MouseButton;
use winit::keyboard::KeyCode;

#[derive(Default)]
pub struct Input {
    keys_down: HashSet<&'static str>,
    keys_pressed: HashSet<&'static str>,
    keys_released: HashSet<&'static str>,
    mouse_down: HashSet<&'static str>,
    mouse_pressed: HashSet<&'static str>,
    mouse_released: HashSet<&'static str>,
    mouse_position: (f64, f64),
    mouse_delta: (f64, f64),
    scroll: f32,
}

impl Input {
    pub fn on_key(&mut self, code: KeyCode, pressed: bool, repeat: bool) {
        let Some(name) = key_name(code) else { return };
        if pressed {
            if !repeat && self.keys_down.insert(name) {
                self.keys_pressed.insert(name);
            }
        } else if self.keys_down.remove(name) {
            self.keys_released.insert(name);
        }
    }

    pub fn on_mouse_button(&mut self, button: MouseButton, pressed: bool) {
        let name = match button {
            MouseButton::Left => "left",
            MouseButton::Right => "right",
            MouseButton::Middle => "middle",
            MouseButton::Back => "back",
            MouseButton::Forward => "forward",
            MouseButton::Other(_) => return,
        };
        if pressed {
            if self.mouse_down.insert(name) {
                self.mouse_pressed.insert(name);
            }
        } else if self.mouse_down.remove(name) {
            self.mouse_released.insert(name);
        }
    }

    pub fn on_cursor_moved(&mut self, x: f64, y: f64) {
        self.mouse_position = (x, y);
    }

    pub fn on_mouse_motion(&mut self, dx: f64, dy: f64) {
        self.mouse_delta.0 += dx;
        self.mouse_delta.1 += dy;
    }

    pub fn on_scroll(&mut self, amount: f32) {
        self.scroll += amount;
    }

    /// Window lost focus: release everything so keys don't get stuck.
    pub fn release_all(&mut self) {
        self.keys_released.extend(self.keys_down.drain());
        self.mouse_released.extend(self.mouse_down.drain());
    }

    pub fn end_frame(&mut self) {
        self.keys_pressed.clear();
        self.keys_released.clear();
        self.mouse_pressed.clear();
        self.mouse_released.clear();
        self.mouse_delta = (0.0, 0.0);
        self.scroll = 0.0;
    }

    pub fn key_down(&self, name: &str) -> bool {
        any_alias(&self.keys_down, name)
    }
    pub fn key_pressed(&self, name: &str) -> bool {
        any_alias(&self.keys_pressed, name)
    }
    pub fn key_released(&self, name: &str) -> bool {
        any_alias(&self.keys_released, name)
    }
    pub fn mouse_down(&self, name: &str) -> bool {
        self.mouse_down.contains(name)
    }
    pub fn mouse_pressed(&self, name: &str) -> bool {
        self.mouse_pressed.contains(name)
    }
    pub fn mouse_released(&self, name: &str) -> bool {
        self.mouse_released.contains(name)
    }
    pub fn mouse_position(&self) -> (f64, f64) {
        self.mouse_position
    }
    pub fn mouse_delta(&self) -> (f64, f64) {
        self.mouse_delta
    }
    pub fn scroll(&self) -> f32 {
        self.scroll
    }
}

/// "shift", "ctrl", "alt" match either side.
fn any_alias(set: &HashSet<&'static str>, name: &str) -> bool {
    match name {
        "shift" => set.contains("lshift") || set.contains("rshift"),
        "ctrl" => set.contains("lctrl") || set.contains("rctrl"),
        "alt" => set.contains("lalt") || set.contains("ralt"),
        _ => set.contains(name),
    }
}

#[rustfmt::skip]
/// Physical key names (layout independent, "w" is the key where W is on QWERTY).
pub fn key_name(code: KeyCode) -> Option<&'static str> {
    use KeyCode::*;
    Some(match code {
        KeyA => "a", KeyB => "b", KeyC => "c", KeyD => "d", KeyE => "e", KeyF => "f",
        KeyG => "g", KeyH => "h", KeyI => "i", KeyJ => "j", KeyK => "k", KeyL => "l",
        KeyM => "m", KeyN => "n", KeyO => "o", KeyP => "p", KeyQ => "q", KeyR => "r",
        KeyS => "s", KeyT => "t", KeyU => "u", KeyV => "v", KeyW => "w", KeyX => "x",
        KeyY => "y", KeyZ => "z",
        Digit0 => "0", Digit1 => "1", Digit2 => "2", Digit3 => "3", Digit4 => "4",
        Digit5 => "5", Digit6 => "6", Digit7 => "7", Digit8 => "8", Digit9 => "9",
        F1 => "f1", F2 => "f2", F3 => "f3", F4 => "f4", F5 => "f5", F6 => "f6",
        F7 => "f7", F8 => "f8", F9 => "f9", F10 => "f10", F11 => "f11", F12 => "f12",
        Space => "space", Enter => "enter", Escape => "escape", Tab => "tab",
        Backspace => "backspace", Delete => "delete", Insert => "insert",
        Home => "home", End => "end", PageUp => "pageup", PageDown => "pagedown",
        ArrowLeft => "left", ArrowRight => "right", ArrowUp => "up", ArrowDown => "down",
        ShiftLeft => "lshift", ShiftRight => "rshift",
        ControlLeft => "lctrl", ControlRight => "rctrl",
        AltLeft => "lalt", AltRight => "ralt",
        Minus => "-", Equal => "=", BracketLeft => "[", BracketRight => "]",
        Semicolon => ";", Quote => "'", Comma => ",", Period => ".", Slash => "/",
        Backslash => "\\", Backquote => "`",
        _ => return None,
    })
}
