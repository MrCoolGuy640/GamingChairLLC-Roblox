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

mod aim;
mod fling;
mod fly;
mod noclip;
mod session;
mod sweep;
mod walk_speed;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{json, Map, Value};

pub use aim::AimCheat;
pub use fling::FlingCheat;
pub use fly::FlyCheat;
pub use noclip::NoClipCheat;
pub use session::SharedSession;
pub use sweep::SweepCheat;
pub use walk_speed::WalkSpeedCheat;

use crate::sdk::roblox::Roblox;

// ---------------------------------------------------------------------------
// Value helpers
// ---------------------------------------------------------------------------

pub fn number(values: &Value, key: &str, default: f64) -> f64 {
    values
        .get(key)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .unwrap_or(default)
}

pub fn boolean(values: &Value, key: &str, default: bool) -> bool {
    values.get(key).and_then(Value::as_bool).unwrap_or(default)
}

pub fn text(values: &Value, key: &str, default: &str) -> String {
    values
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_string()
}

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
        Self::with_values(Value::Object(Map::new()))
    }

    pub fn with_values(values: Value) -> Self {
        Self {
            enabled: AtomicBool::new(false),
            values: Mutex::new(values),
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

    fn on_start(&self, _roblox: &Arc<Roblox>, _values: &Value) -> Result<(), String> {
        Ok(())
    }
    fn on_tick(&self, roblox: &Arc<Roblox>, now: f64);
    fn on_values(&self, _roblox: &Arc<Roblox>, _values: &Value) {}
    fn on_stop(&self, _roblox: &Arc<Roblox>) {}

    /// True exactly once when a one-shot cheat's worker has finished, which
    /// makes the loop switch the cheat back off.
    fn take_finished(&self) -> bool {
        false
    }
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

/// What the loop did to one cheat during a sync pass. The worker turns this
/// into UI events.
#[derive(Debug)]
pub enum SyncOutcome {
    None,
    Started,
    StartFailed(String),
    Finished,
}

/// Mirror the desired state onto the running state. Called by the backend loop
/// whenever the cheat epoch changes (and once at the start of every session).
///
/// This half only starts and live-updates, and the worker calls it in registry
/// order so a dependency (Fly) is running before the cheats that need it.
/// Stopping is a separate, reverse-ordered pass: see `ticked_stop_sync`.
///
/// Start failures disable the cheat instead of leaving the UI showing "on" for
/// something that never ran. A one-shot cheat that has reported completion is
/// stopped and disabled here too, so a sync pass that races the worker cannot
/// resurrect it.
pub fn ticked_apply_sync(cheat: &dyn TickedCheat, roblox: &Arc<Roblox>) -> SyncOutcome {
    let meta = cheat.meta();
    let desired = meta.enabled.load(Ordering::Acquire);
    let running = meta.running.load(Ordering::Acquire);

    if !desired {
        return SyncOutcome::None;
    }

    if running && cheat.take_finished() {
        meta.running.store(false, Ordering::Release);
        meta.enabled.store(false, Ordering::Release);
        stop(cheat, roblox);
        return SyncOutcome::Finished;
    }

    if !running {
        let values = meta.values();
        meta.last_tick.store(0, Ordering::Relaxed);

        return match catch_unwind(AssertUnwindSafe(|| cheat.on_start(roblox, &values))) {
            Ok(Ok(())) => {
                meta.running.store(true, Ordering::Release);
                SyncOutcome::Started
            }
            Ok(Err(err)) => {
                log::warn!(
                    target: "GamingChairLLC.cheats",
                    "Cheat '{}' could not start: {err}",
                    cheat.name()
                );
                meta.enabled.store(false, Ordering::Release);
                SyncOutcome::StartFailed(err)
            }
            Err(_) => {
                log::error!(
                    target: "GamingChairLLC.cheats",
                    "Cheat '{}' panicked in on_start; leaving it disabled",
                    cheat.name()
                );
                meta.enabled.store(false, Ordering::Release);
                SyncOutcome::StartFailed("the cheat panicked while starting".to_string())
            }
        };
    }

    let values = meta.values();
    if catch_unwind(AssertUnwindSafe(|| cheat.on_values(roblox, &values))).is_err() {
        log::error!(
            target: "GamingChairLLC.cheats",
            "Cheat '{}' panicked in on_values",
            cheat.name()
        );
    }
    SyncOutcome::None
}

/// The stop half of a sync pass. The worker calls this in reverse registry
/// order, so Fling and Sweep have released the Fly session before Fly tears it
/// down. Returns `true` when the cheat was actually stopped.
pub fn ticked_stop_sync(cheat: &dyn TickedCheat, roblox: &Arc<Roblox>) -> bool {
    let meta = cheat.meta();
    if meta.enabled.load(Ordering::Acquire) {
        return false;
    }

    if !meta.running.swap(false, Ordering::AcqRel) {
        return false;
    }

    stop(cheat, roblox);
    true
}

fn stop(cheat: &dyn TickedCheat, roblox: &Arc<Roblox>) {
    if catch_unwind(AssertUnwindSafe(|| cheat.on_stop(roblox))).is_err() {
        log::error!(
            target: "GamingChairLLC.cheats",
            "Cheat '{}' panicked in on_stop",
            cheat.name()
        );
    }
}

/// Run the cheat's `on_tick` if it is enabled and its interval allows it.
///
/// Returns `true` when the cheat switched itself off (a one-shot finishing, or
/// a panic), so the caller can mirror that to the UI.
///
/// A panic disables the cheat instead of killing the backend loop; a cheat must
/// never be able to take the whole tool down.
pub fn ticked_tick(cheat: &dyn TickedCheat, roblox: &Arc<Roblox>, now: f64) -> bool {
    let meta = cheat.meta();

    // `running` is only set by `ticked_apply_sync` once `on_start` succeeded
    // and cleared by `on_stop`/`halt`. A disabled cheat must never tick, even
    // though it stays registered in the loop's cheat list.
    if !meta.should_tick() {
        return false;
    }

    // A one-shot worker may finish between sync passes. Stop it here rather
    // than waiting for the next epoch change.
    if cheat.take_finished() {
        meta.running.store(false, Ordering::Release);
        meta.enabled.store(false, Ordering::Release);
        stop(cheat, roblox);
        return true;
    }

    if let Some(interval) = cheat.tick_interval() {
        let last = f64::from_bits(meta.last_tick.load(Ordering::Relaxed));
        if now - last < interval {
            return false;
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
        return true;
    }

    false
}

/// Loop pausing / target lost: undo the effect but keep the desired state.
pub fn ticked_halt(cheat: &dyn TickedCheat, roblox: &Arc<Roblox>) {
    let meta = cheat.meta();
    if !meta.running.swap(false, Ordering::AcqRel) {
        return;
    }

    stop(cheat, roblox);
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

/// One desired-state change produced by `Cheats::plan_toggle`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlannedToggle {
    pub name: &'static str,
    pub enabled: bool,
}

pub struct Cheats {
    toggles: Vec<Arc<dyn ToggleCheat>>,
    ticked: Vec<Arc<dyn TickedCheat>>,
}

impl Cheats {
    pub fn new() -> Self {
        // Registration order is the lifecycle order: Fly starts first (Fling
        // and Sweep borrow its session) and stops last, because stops run in
        // reverse. Aim Assist and NoClip are standalone.
        let shared = SharedSession::new();

        Self {
            toggles: vec![Arc::new(WalkSpeedCheat::new())],
            ticked: vec![
                Arc::new(FlyCheat::new(shared.clone())),
                Arc::new(FlingCheat::new(shared.clone())),
                Arc::new(SweepCheat::new(shared.clone())),
                Arc::new(AimCheat::new(shared)),
                Arc::new(NoClipCheat::new()),
            ],
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

    fn ticked_enabled(&self, name: &str) -> bool {
        self.find_ticked(name)
            .map(|cheat| cheat.meta().enabled())
            .unwrap_or(false)
    }

    /// Resolve a user toggle request into the full set of desired-state
    /// changes, applying the dependency policy:
    ///
    /// * turning Fling or Sweep on forces Fly on first;
    /// * Fly cannot be turned off while Fling is on;
    /// * turning Fly off takes Sweep with it.
    ///
    /// The returned list is in the order the changes should be applied. The
    /// worker does the actual work in registry order (starts) and reverse
    /// (stops), so this only fixes the desired-state flags and the events the
    /// UI sees.
    pub fn plan_toggle(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<Vec<PlannedToggle>, String> {
        let Some(cheat) = self.find_ticked(name) else {
            return if self.find_toggle(name).is_some() {
                Ok(Vec::new())
            } else {
                Err(format!("Cheat '{name}' not found"))
            };
        };
        let name = cheat.name();

        let mut plan = Vec::new();
        match (name, enabled) {
            ("fly", false) => {
                if self.ticked_enabled("fling") {
                    return Err("Turn Fling off before turning Fly off".to_string());
                }
                if self.ticked_enabled("sweep") {
                    plan.push(PlannedToggle {
                        name: "sweep",
                        enabled: false,
                    });
                }
                plan.push(PlannedToggle {
                    name: "fly",
                    enabled: false,
                });
            }
            ("fling" | "sweep", true) => {
                if !self.ticked_enabled("fly") {
                    plan.push(PlannedToggle {
                        name: "fly",
                        enabled: true,
                    });
                }
                plan.push(PlannedToggle { name, enabled: true });
            }
            _ => plan.push(PlannedToggle { name, enabled }),
        }

        Ok(plan)
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
