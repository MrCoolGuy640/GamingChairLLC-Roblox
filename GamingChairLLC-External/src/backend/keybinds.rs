//! Runtime keybinds for the global cheats.
//!
//! Each cheat stores its key in its `values` object (`"keybind": "F1"`), which
//! is what makes the bind part of the active profile for free. This module only
//! *uses* that value: the backend loop polls the key state and toggles the cheat
//! when both gates pass:
//!
//!   1. the foreground window belongs to the attached Roblox process, and
//!   2. the in-game chat input bar is not focused.
//!
//! Keys are edge-triggered: one toggle per press, holding the key does nothing
//! until it is released. A press that happens while a gate is closed is ignored
//! and still counts as "already down", so switching back to the game with the
//! key held cannot fire it.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

use crate::cheats::{self, Cheats};
use crate::events::EventBus;
use crate::sdk::process;
use crate::sdk::roblox::Roblox;

use super::state::BackendState;

/// Key state kept for the lifetime of one backend session.
#[derive(Default)]
pub struct Keybinds {
    held: HashMap<String, HeldKey>,
}

struct HeldKey {
    /// The label the state belongs to. Changing the bind resets the state.
    label: String,
    down: bool,
}

impl Keybinds {
    pub fn new() -> Self {
        Self::default()
    }

    /// One polling pass over every registered cheat.
    ///
    /// Deliberately cheap: only `GetAsyncKeyState` runs per cheat. The
    /// foreground-window and chat-focus checks are expensive (memory reads), so
    /// they run only when a new key-down edge was detected.
    pub fn poll(
        &mut self,
        roblox: &Arc<Roblox>,
        cheats: &Cheats,
        state: &BackendState,
        events: &EventBus,
    ) {
        for cheat in cheats.toggles() {
            let label = keybind_of(&cheat.values());
            if !self.pressed(cheat.name(), &label) || !eligible(roblox) {
                continue;
            }

            let name = cheat.name();
            let enable = !cheat.enabled();
            let result = if enable {
                cheat.enable(roblox, cheat.values())
            } else {
                cheat.disable(roblox)
            };

            match result {
                Ok(()) => {
                    log::info!(
                        target: "GamingChairLLC.keybinds",
                        "Keybind {label}: {name} {}",
                        if enable { "enabled" } else { "disabled" }
                    );
                    events.emit("cheat_state", json!({ "name": name, "enabled": enable }));
                }
                Err(err) => log::warn!(
                    target: "GamingChairLLC.keybinds",
                    "Keybind {label} could not toggle {name}: {err}"
                ),
            }
        }

        for cheat in cheats.ticked() {
            let label = keybind_of(&cheat.meta().values());
            if !self.pressed(cheat.name(), &label) || !eligible(roblox) {
                continue;
            }

            let name = cheat.name();
            let enable = !cheat.meta().enabled();

            if enable {
                cheats::ticked_enable(cheat.as_ref(), None);
            } else {
                cheats::ticked_disable(cheat.as_ref());
            }
            // The loop mirrors desired -> running state on epoch changes.
            state.bump_epoch();

            log::info!(
                target: "GamingChairLLC.keybinds",
                "Keybind {label}: {name} {}",
                if enable { "enabled" } else { "disabled" }
            );
            events.emit("cheat_state", json!({ "name": name, "enabled": enable }));
        }
    }

    /// True exactly once per key-down edge. Returns false for an unbound or
    /// unrecognised key.
    fn pressed(&mut self, name: &str, label: &str) -> bool {
        let Some(vk) = vk_from_label(label) else {
            self.held.remove(name);
            return false;
        };

        let down = key_down(vk);
        let entry = self
            .held
            .entry(name.to_string())
            .or_insert_with(|| HeldKey {
                label: label.to_string(),
                down,
            });

        if entry.label != label {
            // The bind changed while the key state was being tracked. Adopt the
            // new label without firing from a stale "was down" value.
            *entry = HeldKey {
                label: label.to_string(),
                down,
            };
            return false;
        }

        let was_down = entry.down;
        entry.down = down;

        down && !was_down
    }
}

/// Both gates: Roblox owns the foreground window and the chat is not focused.
fn eligible(roblox: &Arc<Roblox>) -> bool {
    if process::foreground_pid() != Some(roblox.pid()) {
        return false;
    }

    !roblox.is_chat_focused()
}

fn key_down(vk: i32) -> bool {
    unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 }
}

fn keybind_of(values: &Value) -> String {
    values
        .get("keybind")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Canonical key label -> Win32 virtual-key code.
///
/// Labels are the ones the UI produces: `A`-`Z`, `0`-`9`, `F1`-`F24`, `Space`,
/// `Ctrl`, `Shift`, `Alt`, `Tab`, `Enter`, `Esc`, `Backspace`, `Delete`,
/// `Insert`, `Home`, `End`, `PageUp`, `PageDown`, `Up`/`Down`/`Left`/`Right`,
/// and the punctuation keys as themselves.
pub fn vk_from_label(label: &str) -> Option<i32> {
    let label = label.trim();
    if label.is_empty() {
        return None;
    }

    // One character: letters, digits and punctuation.
    let mut chars = label.chars();
    if let (Some(ch), None) = (chars.next(), chars.next()) {
        let upper = ch.to_ascii_uppercase();
        if upper.is_ascii_uppercase() || upper.is_ascii_digit() {
            return Some(upper as i32);
        }
        if let Some(vk) = punctuation(ch) {
            return Some(vk);
        }
    }

    // F1..F24.
    if let Some(number) = label.strip_prefix('F').or_else(|| label.strip_prefix('f')) {
        if let Ok(number) = number.parse::<u32>() {
            if (1..=24).contains(&number) {
                return Some(0x70 + number as i32 - 1);
            }
        }
    }

    Some(match label.to_ascii_lowercase().as_str() {
        "space" => 0x20,
        "tab" => 0x09,
        "enter" | "return" => 0x0D,
        "esc" | "escape" => 0x1B,
        "backspace" => 0x08,
        "delete" | "del" => 0x2E,
        "insert" | "ins" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" | "pgup" => 0x21,
        "pagedown" | "pgdn" => 0x22,
        "up" => 0x26,
        "down" => 0x28,
        "left" => 0x25,
        "right" => 0x27,
        "ctrl" | "control" => 0x11,
        "shift" => 0x10,
        "alt" => 0x12,
        "capslock" | "caps" => 0x14,
        _ => return None,
    })
}

fn punctuation(ch: char) -> Option<i32> {
    Some(match ch {
        '-' => 0xBD,
        '=' => 0xBB,
        '[' => 0xDB,
        ']' => 0xDD,
        '\\' => 0xDC,
        ';' => 0xBA,
        '\'' => 0xDE,
        ',' => 0xBC,
        '.' => 0xBE,
        '/' => 0xBF,
        '`' => 0xC0,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::vk_from_label;

    #[test]
    fn letters_digits_and_function_keys_map_to_vk_codes() {
        assert_eq!(vk_from_label("A"), Some(0x41));
        assert_eq!(vk_from_label("z"), Some(0x5A));
        assert_eq!(vk_from_label("0"), Some(0x30));
        assert_eq!(vk_from_label("9"), Some(0x39));
        assert_eq!(vk_from_label("F1"), Some(0x70));
        assert_eq!(vk_from_label("F12"), Some(0x7B));
        assert_eq!(vk_from_label("f24"), Some(0x87));
    }

    #[test]
    fn named_keys_and_punctuation_map_to_vk_codes() {
        assert_eq!(vk_from_label("Space"), Some(0x20));
        assert_eq!(vk_from_label("Esc"), Some(0x1B));
        assert_eq!(vk_from_label("Backspace"), Some(0x08));
        assert_eq!(vk_from_label("Up"), Some(0x26));
        assert_eq!(vk_from_label("-"), Some(0xBD));
        assert_eq!(vk_from_label("`"), Some(0xC0));
    }

    #[test]
    fn empty_or_unknown_labels_are_unbound() {
        assert_eq!(vk_from_label(""), None);
        assert_eq!(vk_from_label("   "), None);
        assert_eq!(vk_from_label("F25"), None);
        assert_eq!(vk_from_label("Hyper"), None);
    }
}
