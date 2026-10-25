//! NoClip: continuously clears `CanCollide` on the local character's parts.
//!
//! The game (and the server's physics replication) periodically restores
//! collision, so the flag is re-applied on a short interval. Disabling the
//! cheat restores collision on every part of the character.

use std::sync::Arc;

use serde_json::Value;

use super::{TickedCheat, TickedMeta};
use crate::sdk::roblox::{Part, Roblox};

/// How often collision is re-cleared.
const TICK_INTERVAL: f64 = 1.0;

const PART_CLASSES: &[&str] = &[
    "part",
    "basepart",
    "meshpart",
    "unionoperation",
    "trusspart",
    "cornerwedgepart",
    "wedgepart",
    "seat",
    "vehicleseat",
];

pub struct NoClipCheat {
    meta: TickedMeta,
}

impl NoClipCheat {
    pub fn new() -> Self {
        Self {
            meta: TickedMeta::new(),
        }
    }

    fn apply(roblox: &Arc<Roblox>, noclip: bool) {
        let Some(character) = roblox.get_local_character() else {
            return;
        };

        for child in character.get_children() {
            let class_name = child.get_class_name().to_ascii_lowercase();
            if !PART_CLASSES.contains(&class_name.as_str()) {
                continue;
            }

            let part = Part::new(child);
            part.set_can_collide(!noclip);
        }
    }
}

impl Default for NoClipCheat {
    fn default() -> Self {
        Self::new()
    }
}

impl TickedCheat for NoClipCheat {
    fn name(&self) -> &'static str {
        "noclip"
    }

    fn display_name(&self) -> &'static str {
        "NoClip"
    }

    fn description(&self) -> &'static str {
        "Walk through walls by disabling collision on your character"
    }

    fn meta(&self) -> &TickedMeta {
        &self.meta
    }

    fn tick_interval(&self) -> Option<f64> {
        Some(TICK_INTERVAL)
    }

    fn on_start(&self, roblox: &Arc<Roblox>, _values: &Value) {
        Self::apply(roblox, true);
    }

    fn on_tick(&self, roblox: &Arc<Roblox>, _now: f64) {
        Self::apply(roblox, true);
    }

    fn on_stop(&self, roblox: &Arc<Roblox>) {
        Self::apply(roblox, false);
    }
}
