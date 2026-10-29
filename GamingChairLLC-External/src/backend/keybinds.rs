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

use crate::cheats::{self, Cheats};
use crate::events::EventBus;
use crate::sdk::input::{key_down, vk_from_label};
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
                let values = cheat.values();
                // While dead / on the home screen there is no humanoid. Keep
                // the switch on so the resume pass applies it; a failed
                // `enable` here would silently drop the user's keypress.
                let no_character = roblox.get_local_humanoid().is_none();
                if (state.hacks_suspended() || no_character)
                    && cheat.defer_enable(values.clone())
                {
                    Ok(())
                } else {
                    cheat.enable(roblox, values)
                }
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
            let plan = match cheats.plan_toggle(name, enable) {
                Ok(plan) => plan,
                Err(err) => {
                    log::info!(
                        target: "GamingChairLLC.keybinds",
                        "Keybind {label}: cannot turn {name} {}: {err}",
                        if enable { "on" } else { "off" }
                    );
                    continue;
                }
            };

            for change in plan {
                let Some(target) = cheats.find_ticked(change.name) else {
                    continue;
                };

                if change.enabled {
                    cheats::ticked_enable(target.as_ref(), None);
                } else {
                    cheats::ticked_disable(target.as_ref());
                }

                // A forced prerequisite (Fly, when Fling or Sweep comes on)
                // has to reach the UI too, or its switch would stay off.
                events.emit(
                    "cheat_state",
                    json!({ "name": change.name, "enabled": change.enabled }),
                );
            }

            // The loop mirrors desired -> running state on epoch changes.
            state.bump_epoch();

            log::info!(
                target: "GamingChairLLC.keybinds",
                "Keybind {label}: {name} {}",
                if enable { "enabled" } else { "disabled" }
            );
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

fn keybind_of(values: &Value) -> String {
    values
        .get("keybind")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}
