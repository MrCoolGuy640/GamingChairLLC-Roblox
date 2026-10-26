//! Cheat module system.
//!
//! Two kinds of cheat:
//!
//! * `ToggleCheat` – runs once when switched on and once when switched off.
//!   It writes a value and forgets about it (WalkSpeed).
//! * `TickedCheat` – runs continuously on the backend loop while enabled.
//!   It re-applies an effect every iteration (NoClip).
//!
//! Both use the same `Arc<Roblox>` connection. The UI-facing state lives on the
//! cheat object; the backend thread mirrors the *desired* state onto its
//! *running* state via `ticked_apply_sync`.

mod noclip;
mod walk_speed;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{json, Map, Value};

pub use noclip::NoClipCheat;
pub use walk_speed::WalkSpeedCheat;

use crate::sdk::roblox::Roblox;

// ---------------------------------------------------------------------------
// Toggle cheats
// ---------------------------------------------------------------------------

pub trait ToggleCheat: Send + Sync {
    fn name(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn description(&self) -> &'static str;

    fn enabled(&self) -> bool;
    fn values(&self) -> Value;
    fn store_values(&self, values: &Value);

    fn enable(&self, roblox: &Arc<Roblox>, values: Value) -> Result<(), String>;
    fn disable(&self, roblox: &Arc<Roblox>) -> Result<(), String>;

    /// Live-update. Disabled cheats only remember the new values.
    fn update_values(
        &self,
        roblox: Option<&Arc<Roblox>>,
        values: Value,
    ) -> Result<(), String> {
        self.store_values(&values);

        if self.enabled() {
            let roblox = roblox.ok_or_else(|| "Not attached".to_string())?;
            self.enable(roblox, self.values())
        } else {
            Ok(())
        }
    }

    /// Clear the enabled flag without touching the game (target lost).
    fn reset(&self);
}

pub fn toggle_info(cheat: &dyn ToggleCheat) -> Value {
    json!({
        "name": cheat.name(),
        "display_name": cheat.display_name(),
        "description": cheat.description(),
        "type": "toggle",
        "enabled": cheat.enabled(),
        "values": cheat.values(),
    })
}

// ---------------------------------------------------------------------------
// Ticked cheats
// ---------------------------------------------------------------------------

pub struct TickedMeta {
    enabled: AtomicBool,
    values: Mutex<Value>,
    running: AtomicBool,
    last_tick: AtomicU64,
}

impl TickedMeta {
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            values: Mutex::new(Value::Object(Map::new())),
            running: AtomicBool::new(false),
            last_tick: AtomicU64::new(0),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn values(&self) -> Value {
        self.values.lock().clone()
    }

    /// A cheat may only tick while it is enabled *and* has been started by
    /// `ticked_apply_sync`. Checking both closes the window between "the user
    /// turned it off" and "the loop ran `on_stop`", during which a tick would
    /// otherwise re-apply the effect that `on_stop` is about to remove.
    pub fn should_tick(&self) -> bool {
        self.running.load(Ordering::Acquire) && self.enabled.load(Ordering::Acquire)
    }
}

impl Default for TickedMeta {
    fn default() -> Self {
        Self::new()
    }
}

pub trait TickedCheat: Send + Sync {
    fn name(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn description(&self) -> &'static str;

    fn meta(&self) -> &TickedMeta;

    /// `None` runs every loop iteration. `Some(seconds)` throttles the cheat.
    fn tick_interval(&self) -> Option<f64> {
        None
    }

    fn on_start(&self, _roblox: &Arc<Roblox>, _values: &Value) {}
    fn on_tick(&self, roblox: &Arc<Roblox>, now: f64);
    fn on_values(&self, _roblox: &Arc<Roblox>, _values: &Value) {}
    fn on_stop(&self, _roblox: &Arc<Roblox>) {}
}

pub fn ticked_info(cheat: &dyn TickedCheat) -> Value {
    json!({
        "name": cheat.name(),
        "display_name": cheat.display_name(),
        "description": cheat.description(),
        "type": "ticked",
        "enabled": cheat.meta().enabled(),
        "values": cheat.meta().values(),
    })
}

pub fn ticked_enable(cheat: &dyn TickedCheat, values: Option<Value>) {
    let meta = cheat.meta();

    if let Some(values) = values {
        merge_values(&mut meta.values.lock(), values);
    }

    meta.enabled.store(true, Ordering::Release);
}

pub fn ticked_disable(cheat: &dyn TickedCheat) {
    cheat.meta().enabled.store(false, Ordering::Release);
}

pub fn ticked_set_values(cheat: &dyn TickedCheat, values: Value) {
    merge_values(&mut cheat.meta().values.lock(), values);
}

/// Mirror the desired state onto the running state. Called by the backend loop
/// whenever the cheat epoch changes (and once at the start of every session).
pub fn ticked_apply_sync(cheat: &dyn TickedCheat, roblox: &Arc<Roblox>) {
    let meta = cheat.meta();
    let desired = meta.enabled.load(Ordering::Acquire);
    let running = meta.running.load(Ordering::Acquire);

    if desired && !running {
        let values = meta.values();
        meta.last_tick.store(0, Ordering::Relaxed);

        let started = catch_unwind(AssertUnwindSafe(|| cheat.on_start(roblox, &values)));
        if started.is_ok() {
            meta.running.store(true, Ordering::Release);
        } else {
            log::error!(
                target: "GamingChairLLC.cheats",
                "Cheat '{}' panicked in on_start; leaving it disabled",
                cheat.name()
            );
            meta.enabled.store(false, Ordering::Release);
        }
    } else if desired {
        let values = meta.values();
        if catch_unwind(AssertUnwindSafe(|| cheat.on_values(roblox, &values))).is_err() {
            log::error!(
                target: "GamingChairLLC.cheats",
                "Cheat '{}' panicked in on_values",
                cheat.name()
            );
        }
    } else if running {
        meta.running.store(false, Ordering::Release);
        if catch_unwind(AssertUnwindSafe(|| cheat.on_stop(roblox))).is_err() {
            log::error!(
                target: "GamingChairLLC.cheats",
                "Cheat '{}' panicked in on_stop",
                cheat.name()
            );
        }
    }
}

/// Run the cheat's `on_tick` if it is enabled and its interval allows it.
///
/// A panic disables the cheat instead of killing the backend loop; a cheat must
/// never be able to take the whole tool down.
pub fn ticked_tick(cheat: &dyn TickedCheat, roblox: &Arc<Roblox>, now: f64) {
    let meta = cheat.meta();

    // `running` is only set by `ticked_apply_sync` once `on_start` succeeded
    // and cleared by `on_stop`/`halt`. A disabled cheat must never tick, even
    // though it stays registered in the loop's cheat list.
    if !meta.should_tick() {
        return;
    }

    if let Some(interval) = cheat.tick_interval() {
        let last = f64::from_bits(meta.last_tick.load(Ordering::Relaxed));
        if now - last < interval {
            return;
        }
    }

    meta.last_tick.store(now.to_bits(), Ordering::Relaxed);

    if catch_unwind(AssertUnwindSafe(|| cheat.on_tick(roblox, now))).is_err() {
        log::error!(
            target: "GamingChairLLC.cheats",
            "Cheat '{}' panicked in on_tick; disabling it",
            cheat.name()
        );
        meta.running.store(false, Ordering::Release);
        meta.enabled.store(false, Ordering::Release);
    }
}

/// Loop pausing / target lost: undo the effect but keep the desired state.
pub fn ticked_halt(cheat: &dyn TickedCheat, roblox: &Arc<Roblox>) {
    let meta = cheat.meta();
    if !meta.running.swap(false, Ordering::AcqRel) {
        return;
    }

    if catch_unwind(AssertUnwindSafe(|| cheat.on_stop(roblox))).is_err() {
        log::error!(
            target: "GamingChairLLC.cheats",
            "Cheat '{}' panicked in on_stop while halting",
            cheat.name()
        );
    }
}

pub fn ticked_reset(cheat: &dyn TickedCheat) {
    let meta = cheat.meta();
    meta.enabled.store(false, Ordering::Release);
    meta.running.store(false, Ordering::Release);
}

fn merge_values(slot: &mut Value, new_values: Value) {
    match (slot, new_values) {
        (Value::Object(current), Value::Object(incoming)) => {
            for (key, value) in incoming {
                current.insert(key, value);
            }
        }
        (slot, new_values) => *slot = new_values,
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

pub struct Cheats {
    toggles: Vec<Arc<dyn ToggleCheat>>,
    ticked: Vec<Arc<dyn TickedCheat>>,
}

impl Cheats {
    pub fn new() -> Self {
        Self {
            toggles: vec![Arc::new(WalkSpeedCheat::new())],
            ticked: vec![Arc::new(NoClipCheat::new())],
        }
    }

    pub fn toggles(&self) -> &[Arc<dyn ToggleCheat>] {
        &self.toggles
    }

    pub fn ticked(&self) -> &[Arc<dyn TickedCheat>] {
        &self.ticked
    }

    pub fn find_toggle(&self, name: &str) -> Option<&Arc<dyn ToggleCheat>> {
        self.toggles.iter().find(|cheat| cheat.name() == name)
    }

    pub fn find_ticked(&self, name: &str) -> Option<&Arc<dyn TickedCheat>> {
        self.ticked.iter().find(|cheat| cheat.name() == name)
    }

    pub fn info(&self) -> Value {
        let mut map = Map::new();

        for cheat in &self.toggles {
            map.insert(cheat.name().to_string(), toggle_info(cheat.as_ref()));
        }
        for cheat in &self.ticked {
            map.insert(cheat.name().to_string(), ticked_info(cheat.as_ref()));
        }

        Value::Object(map)
    }

    /// Every cheat's current accepted values, keyed by cheat name. This is what
    /// gets written into a profile.
    pub fn values_snapshot(&self) -> Value {
        let mut map = Map::new();

        for cheat in &self.toggles {
            map.insert(cheat.name().to_string(), cheat.values());
        }
        for cheat in &self.ticked {
            map.insert(cheat.name().to_string(), cheat.meta().values());
        }

        Value::Object(map)
    }

    /// Store values from a profile without enabling anything or touching the
    /// game. Used when a profile is loaded before attaching.
    pub fn apply_values(&self, values: &Value) {
        let Some(values) = values.as_object() else {
            return;
        };

        for cheat in &self.toggles {
            if let Some(value) = values.get(cheat.name()) {
                cheat.store_values(value);
            }
        }
        for cheat in &self.ticked {
            if let Some(value) = values.get(cheat.name()) {
                ticked_set_values(cheat.as_ref(), value.clone());
            }
        }
    }
}

impl Default for Cheats {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::TickedMeta;

    #[test]
    fn a_cheat_only_ticks_while_enabled_and_running() {
        let meta = TickedMeta::new();

        // Freshly registered: neither enabled nor started.
        assert!(!meta.should_tick());

        // Enabled by the UI, but the backend loop has not started it yet.
        meta.enabled.store(true, std::sync::atomic::Ordering::Release);
        assert!(!meta.should_tick());

        // Started.
        meta.running.store(true, std::sync::atomic::Ordering::Release);
        assert!(meta.should_tick());

        // Toggled off: must stop immediately, even before `on_stop` runs.
        meta.enabled.store(false, std::sync::atomic::Ordering::Release);
        assert!(!meta.should_tick());

        // Toggled back on while `running` is still true (no `on_stop` ran):
        // the loop syncs and resumes ticking.
        meta.enabled.store(true, std::sync::atomic::Ordering::Release);
        assert!(meta.should_tick());
    }

    #[test]
    fn a_halted_cheat_does_not_tick() {
        let meta = TickedMeta::new();
        meta.enabled.store(true, std::sync::atomic::Ordering::Release);
        meta.running.store(true, std::sync::atomic::Ordering::Release);

        meta.running.store(false, std::sync::atomic::Ordering::Release);
        assert!(!meta.should_tick());
    }
}
