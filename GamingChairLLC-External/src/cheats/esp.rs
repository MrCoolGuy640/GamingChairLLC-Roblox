//! ESP: outline other players through walls with the engine's own `Highlight`.
//!
//! The highlight is a real engine object created with the reflective
//! `Instance.new` runtime and parented to the character model, which is what the
//! Python proof-of-concept did. One highlight is kept per character; the cheat
//! tracks the objects it created so it can remove them again without relying on
//! their `Name` (the SDK's `set_name` points `NameContainer` at a buffer the
//! engine does not own, so using it here would crash the client once the engine
//! destroys the character).
//!
//! The character list is re-checked on a slow tick: creating instances is
//! comparatively expensive and the client only gains/loses players every so
//! often.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{json, Value};

use super::{boolean, number, text, TickedCheat, TickedMeta};
use crate::sdk::roblox::{Instance, Roblox};

/// How often characters are re-scanned. Matches the Python watcher.
const TICK_INTERVAL: f64 = 2.0;

const HIGHLIGHT_CLASS: &str = "Highlight";

/// Only method for now. The dropdown exists so future methods (and their own
/// settings) can be added without changing the shape of the stored values.
const METHOD_NATIVE_HIGHLIGHT: &str = "native_highlight";

pub struct EspCheat {
    meta: TickedMeta,
    /// Character model address -> highlight instance address, for the
    /// highlights this cheat created.
    highlights: Mutex<HashMap<u64, u64>>,
}

impl EspCheat {
    pub fn new() -> Self {
        Self {
            meta: TickedMeta::with_values(default_values()),
            highlights: Mutex::new(HashMap::new()),
        }
    }

    /// Bring the game in line with `values`: remove stale highlights, create
    /// missing ones and re-apply the appearance.
    ///
    /// Returns the first failure that is worth surfacing (the call gate not
    /// being available, or a highlight that could not be parented). Per-tick
    /// callers ignore it; `on_start` reports it so the UI does not show a
    /// switch that never ran.
    fn sync(&self, roblox: &Arc<Roblox>, values: &Value) -> Result<(), String> {
        if text(values, "method", METHOD_NATIVE_HIGHLIGHT) != METHOD_NATIVE_HIGHLIGHT {
            self.clear(roblox);
            return Ok(());
        }

        let fill = number(values, "fill_transparency", 0.5).clamp(0.0, 1.0) as f32;
        let outline = number(values, "outline_transparency", 0.0).clamp(0.0, 1.0) as f32;
        let highlight_local = boolean(values, "highlight_local", false);

        // No DataModel yet (loading screen): leave the tracked map alone, the
        // pruner below would not be able to tell "gone" from "unreadable".
        let Some(workspace) = roblox.get_workspace() else {
            return Ok(());
        };

        let mut wanted: Vec<(u64, Instance)> = Vec::new();
        let mut unresolved: Vec<String> = Vec::new();

        for player in roblox.get_player_list() {
            if player.get_class_name() != "Player" {
                continue;
            }

            if !highlight_local && player.is_local_player() {
                continue;
            }

            match player.character() {
                Some(character) => wanted.push((character.address(), character)),
                // Some dumps do not document the character pointer; fall back
                // to the name lookup the Python watcher used.
                None => {
                    let name = player.get_name();
                    if !name.is_empty() {
                        unresolved.push(name);
                    }
                }
            }
        }

        if !unresolved.is_empty() {
            let children = workspace.get_children();
            for name in unresolved {
                if let Some(character) = children
                    .iter()
                    .find(|child| child.get_name() == name && child.get_class_name() == "Model")
                {
                    wanted.push((character.address(), character.clone()));
                }
            }
        }

        let mut tracked = self.highlights.lock();
        let mut error: Option<String> = None;

        // Characters that left (or respawned under a new model) first: their
        // highlight is still parented, so it is usually safe to unparent.
        let characters: Vec<u64> = tracked.keys().copied().collect();
        for character in characters {
            let highlight = tracked[&character];
            let still_wanted = wanted.iter().any(|(address, _)| *address == character);

            if !still_wanted {
                tracked.remove(&character);
                release(roblox, highlight);
            } else if !is_live(roblox, highlight, character) {
                // The engine destroyed it along with the character; forget it
                // without writing to what may now be a different object.
                tracked.remove(&character);
            }
        }

        for (character_address, character) in &wanted {
            if tracked.contains_key(character_address) {
                continue;
            }

            let highlight = match roblox.instance_new().new_instance(HIGHLIGHT_CLASS) {
                Ok(highlight) => highlight,
                Err(err) => {
                    // The gate is shared, so the remaining characters would
                    // fail the same way.
                    error.get_or_insert(err.to_string());
                    break;
                }
            };

            set_style(roblox, &highlight, fill, outline);

            match roblox.instance_new().set_parent(&highlight, Some(character)) {
                Ok(()) => {
                    tracked.insert(*character_address, highlight.address());
                    log::debug!(
                        target: "GamingChairLLC.esp",
                        "Highlight {:#x} parented to character {:#x}",
                        highlight.address(),
                        character_address
                    );
                }
                Err(err) => {
                    let _ = roblox.instance_new().unparent(&highlight);
                    error.get_or_insert_with(|| format!("could not parent a highlight: {err}"));
                }
            }
        }

        for (character, highlight) in tracked.iter() {
            if is_live(roblox, *highlight, *character) {
                set_style(roblox, &roblox.instance(*highlight), fill, outline);
            }
        }

        match error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    /// Unparent everything this cheat created.
    fn clear(&self, roblox: &Arc<Roblox>) {
        let mut tracked = self.highlights.lock();

        for (_, highlight) in tracked.drain() {
            release(roblox, highlight);
        }
    }
}

impl Default for EspCheat {
    fn default() -> Self {
        Self::new()
    }
}

impl TickedCheat for EspCheat {
    fn name(&self) -> &'static str {
        "esp"
    }

    fn display_name(&self) -> &'static str {
        "ESP"
    }

    fn description(&self) -> &'static str {
        "Outline other players through walls"
    }

    fn meta(&self) -> &TickedMeta {
        &self.meta
    }

    fn tick_interval(&self) -> Option<f64> {
        Some(TICK_INTERVAL)
    }

    fn on_start(&self, roblox: &Arc<Roblox>, values: &Value) -> Result<(), String> {
        self.sync(roblox, values)
    }

    fn on_tick(&self, roblox: &Arc<Roblox>, _now: f64) {
        let values = self.meta.values();
        if let Err(err) = self.sync(roblox, &values) {
            log::debug!(
                target: "GamingChairLLC.esp",
                "ESP tick could not complete: {err}"
            );
        }
    }

    fn on_stop(&self, roblox: &Arc<Roblox>) {
        self.clear(roblox);
    }
}

fn is_live(roblox: &Arc<Roblox>, highlight: u64, character: u64) -> bool {
    if highlight == 0 {
        return false;
    }

    let instance = roblox.instance(highlight);
    instance.get_class_name() == HIGHLIGHT_CLASS && instance.parent_address() == character
}

fn release(roblox: &Arc<Roblox>, highlight: u64) {
    if highlight == 0 || !roblox.instance_new().is_ready() {
        return;
    }

    let instance = roblox.instance(highlight);
    if instance.get_class_name() != HIGHLIGHT_CLASS {
        return;
    }

    if let Err(err) = roblox.instance_new().unparent(&instance) {
        log::debug!(
            target: "GamingChairLLC.esp",
            "Could not remove highlight {highlight:#x}: {err}"
        );
    }
}

fn set_style(roblox: &Arc<Roblox>, highlight: &Instance, fill: f32, outline: f32) {
    let offsets = &roblox.context().offsets.highlight;

    highlight.write_float(offsets.fill_transparency, fill);
    highlight.write_float(offsets.outline_transparency, outline);
}

pub fn default_values() -> Value {
    json!({
        "keybind": "H",
        "method": METHOD_NATIVE_HIGHLIGHT,
        "highlight_local": false,
        "fill_transparency": 0.5,
        "outline_transparency": 0.0,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Manual end-to-end check against a live client: install the gate, create
    /// real `Highlight` objects, parent them and remove them again. This is the
    /// exact path the cheat takes, so a crash here is a crash in the ESP.
    ///
    /// `cargo test --offline live_esp_roundtrip -- --ignored --nocapture`
    #[test]
    #[ignore = "requires a live Roblox client in a running place"]
    fn live_esp_roundtrip() {
        let paths = crate::config::Paths::discover();
        let offsets = Arc::new(crate::sdk::offsets::load(&paths).expect("offsets"));
        let roblox = Roblox::connect(offsets).expect("connect");

        // The style offsets must point at the engine's own transparency fields.
        // A fresh Highlight reports FillTransparency = 0.5 and
        // OutlineTransparency = 0 before anything writes to it; if these reads
        // look wrong, `set_style` would be corrupting an unrelated field.
        let probe = roblox
            .instance_new()
            .new_instance(HIGHLIGHT_CLASS)
            .expect("probe highlight");
        let highlight = &roblox.context().offsets.highlight;
        let memory = &roblox.context().memory;
        let fill = memory.read_float(probe.address() + highlight.fill_transparency);
        let outline = memory.read_float(probe.address() + highlight.outline_transparency);
        println!("fresh Highlight: fill={fill} outline={outline}");
        assert!((fill - 0.5).abs() < 0.01, "FillTransparency offset is wrong");
        assert!(outline.abs() < 0.01, "OutlineTransparency offset is wrong");

        let esp = EspCheat::new();
        let mut values = default_values();
        // Include the local player so this works in a solo place.
        values["highlight_local"] = json!(true);

        esp.on_start(&roblox, &values).expect("esp start");

        let created = esp.highlights.lock().len();
        println!("ESP created {created} highlight(s)");

        std::thread::sleep(Duration::from_secs(2));

        esp.on_stop(&roblox);
        println!("ESP stopped cleanly");

        assert!(created > 0, "no character was highlighted");
    }
}
