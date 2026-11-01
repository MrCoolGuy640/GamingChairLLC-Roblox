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
use crate::sdk::roblox::{player::Player, Instance, Roblox};

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
    /// The `(fill, outline, highlight_local)` settings currently baked into the
    /// engine objects. Highlight styling is copied into the engine's render
    /// object at creation, so a style change has to rebuild the highlights.
    settings: Mutex<Option<(f32, f32, bool)>>,
}

impl EspCheat {
    pub fn new() -> Self {
        Self {
            meta: TickedMeta::with_values(default_values()),
            highlights: Mutex::new(HashMap::new()),
            settings: Mutex::new(None),
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

        // Slider values are opacity: 0 = fully transparent, 1 = fully
        // coloured. Roblox stores the inverse (`FillTransparency` /
        // `OutlineTransparency`).
        let (fill, outline, highlight_local) = settings_from(values);

        // No DataModel yet (loading screen): leave the tracked map alone, the
        // pruner below would not be able to tell "gone" from "unreadable".
        let Some(workspace) = roblox.get_workspace() else {
            return Ok(());
        };

        let players = roblox.get_player_list();
        if players.is_empty() {
            // A respawn (or a transient read failure) can empty this list.
            // Treating that as "everyone left" is what used to unparent every
            // highlight in the game; an empty list carries no information.
            log::debug!(
                target: "GamingChairLLC.esp",
                "Player list is empty; keeping the existing highlights"
            );
            return Ok(());
        }

        // Highlight styling is copied into the engine's render object when the
        // instance is created; rewriting the fields of an already-parented
        // highlight does not re-render it. Rebuild the objects when either
        // style value changes — this is what switching ESP off and on used to
        // do by hand.
        let settings = (fill, outline, highlight_local);
        let style_changed = {
            let mut last = self.settings.lock();
            let previous = last.replace(settings);
            previous.map_or(true, |previous| previous.0 != fill || previous.1 != outline)
        };

        // `LocalPlayer` lives on the Players service, never on a Player
        // instance. Compare against the DataModel's pointer, and fall back to
        // the name when the pointer is temporarily unreadable.
        let local = roblox.get_local_player();
        let local_address = local.as_ref().map(|player| player.address());
        let local_name = local
            .as_ref()
            .map(|player| player.get_name())
            .unwrap_or_default();

        let mut wanted: Vec<(u64, Instance)> = Vec::new();

        for player in &players {
            if player.get_class_name() != "Player" {
                continue;
            }

            // Addresses are exact; the name is a second, independent check so a
            // stale LocalPlayer pointer can never make the toggle leak a
            // highlight onto the local character.
            let is_local = local_address == Some(player.address())
                || (!local_name.is_empty() && player.get_name() == local_name);
            if !highlight_local && is_local {
                continue;
            }

            // A joining/respawning player has no model yet; skip just that
            // player for this tick instead of distrusting the whole list.
            if let Some(character) = current_character(player, &workspace) {
                wanted.push((character.address(), character));
            }
        }

        let mut tracked = self.highlights.lock();
        let mut error: Option<String> = None;

        if style_changed && !tracked.is_empty() {
            for (_, highlight) in tracked.drain() {
                release(roblox, highlight);
            }
        }

        // Drop entries whose character model left Workspace *and* whose
        // Highlight is gone with it. A respawn or a player leaving destroys the
        // model, so there is nothing to unparent and every other character's
        // highlight must stay untouched. Keeping the entry when either check
        // still sees the object avoids pruning on a partial/throttled read.
        let children = workspace.get_children();
        if !children.is_empty() {
            tracked.retain(|character, highlight| {
                children.iter().any(|child| child.address() == *character)
                    || is_live(roblox, *highlight, *character)
            });
        }

        // With the toggle off, clear the highlight from the local player's
        // current character *and* from a previous body that is still in
        // Workspace. Name matching catches the dead body; the retain above
        // catches models the engine already destroyed.
        if !highlight_local && !local_name.is_empty() {
            let stale: Vec<u64> = tracked
                .keys()
                .copied()
                .filter(|address| !wanted.iter().any(|(wanted, _)| wanted == address))
                .filter(|address| roblox.instance(*address).get_name() == local_name)
                .collect();

            for character in stale {
                if let Some(highlight) = tracked.remove(&character) {
                    release(roblox, highlight);
                }
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
        {
            let mut tracked = self.highlights.lock();

            for (_, highlight) in tracked.drain() {
                release(roblox, highlight);
            }
        }

        // The next start has to build fresh objects, whatever the style is.
        *self.settings.lock() = None;
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

    /// Slider release (and the local-player switch) bump the values epoch, so
    /// apply the change right away instead of waiting for the next 2 s tick.
    fn on_values(&self, roblox: &Arc<Roblox>, values: &Value) {
        let settings = settings_from(values);
        if self.settings.lock().as_ref() == Some(&settings) {
            return;
        }

        if let Err(err) = self.sync(roblox, values) {
            log::debug!(
                target: "GamingChairLLC.esp",
                "ESP update could not complete: {err}"
            );
        }
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

/// The character model a player currently controls.
///
/// Prefers the direct `Player.Character`/`ModelInstance` pointer and falls back
/// to the player's name under Workspace, which is the lookup the Python watcher
/// used. Returning `None` means "not spawned yet", never "player is gone".
fn current_character(player: &Player, workspace: &Instance) -> Option<Instance> {
    if let Some(character) = player.character() {
        if character.address() != 0 {
            return Some(character);
        }
    }

    let name = player.get_name();
    if name.is_empty() {
        return None;
    }

    workspace
        .get_children()
        .into_iter()
        .find(|child| child.get_name() == name && child.get_class_name() == "Model")
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

/// The slider is opacity (0 = fully transparent, 1 = fully coloured) while
/// Roblox's fields are transparency (0 = fully opaque, 1 = invisible).
fn engine_transparency(opacity: f64) -> f32 {
    1.0 - opacity.clamp(0.0, 1.0) as f32
}

/// The settings that decide what the engine objects look like.
fn settings_from(values: &Value) -> (f32, f32, bool) {
    (
        engine_transparency(number(values, "fill_opacity", 0.5)),
        engine_transparency(number(values, "outline_opacity", 1.0)),
        boolean(values, "highlight_local", false),
    )
}

pub fn default_values() -> Value {
    json!({
        "keybind": "H",
        "method": METHOD_NATIVE_HIGHLIGHT,
        "highlight_local": false,
        "fill_opacity": 0.5,
        "outline_opacity": 1.0,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn opacity_is_written_as_inverted_engine_transparency() {
        assert_eq!(engine_transparency(1.0), 0.0, "1 = fully coloured");
        assert_eq!(engine_transparency(0.0), 1.0, "0 = fully transparent");
        assert!((engine_transparency(0.2) - 0.8).abs() < 1e-6);
        assert_eq!(engine_transparency(2.0), 0.0, "clamped");
        assert_eq!(engine_transparency(-1.0), 1.0, "clamped");
    }

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

        let local_character = roblox
            .get_local_character()
            .expect("local character")
            .address();

        esp.on_start(&roblox, &values).expect("esp start");

        let created = esp.highlights.lock().len();
        println!("ESP created {created} highlight(s)");
        assert!(
            esp.highlights.lock().contains_key(&local_character),
            "toggle on did not highlight the local player"
        );

        // Turning the toggle off must remove the local highlight while leaving
        // every other player's highlight alone.
        values["highlight_local"] = json!(false);
        esp.sync(&roblox, &values).expect("toggle off");
        assert!(
            !esp.highlights.lock().contains_key(&local_character),
            "toggle off left the local highlight parented"
        );
        println!(
            "toggle off removed the local highlight; {} remain",
            esp.highlights.lock().len()
        );

        // Turning it back on must re-add exactly the local player's highlight.
        values["highlight_local"] = json!(true);
        esp.sync(&roblox, &values).expect("toggle on");
        assert!(
            esp.highlights.lock().contains_key(&local_character),
            "toggle on did not re-add the local highlight"
        );

        // Slider release path (`on_values`): changing the style must rebuild
        // the engine objects, and the new opacity must be baked into them.
        let before = esp.highlights.lock().get(&local_character).copied();
        values["fill_opacity"] = json!(0.15);
        esp.on_values(&roblox, &values);
        let after = esp.highlights.lock().get(&local_character).copied();
        let fill = after.map(|address| memory.read_float(address + highlight.fill_transparency));
        println!("style change: local {before:?} -> {after:?} (fill transparency {fill:?})");
        assert!(after.is_some(), "style change removed the local highlight");
        assert!(
            (fill.unwrap() - 0.85).abs() < 0.01,
            "style change did not apply: {fill:?}"
        );

        std::thread::sleep(Duration::from_secs(2));

        esp.on_stop(&roblox);
        println!("ESP stopped cleanly");

        assert!(created > 0, "no character was highlighted");
    }
}
