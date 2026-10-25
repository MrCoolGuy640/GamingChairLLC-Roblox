//! WalkSpeed: a toggle cheat that writes the humanoid's speed once.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value;

use super::{merge_values, ToggleCheat};
use crate::sdk::roblox::Roblox;

const DEFAULT_SPEED: f32 = 16.0;

pub struct WalkSpeedCheat {
    enabled: AtomicBool,
    values: Mutex<Value>,
    /// The game's own speed, captured the first time the cheat is applied.
    original: Mutex<Option<f32>>,
}

impl WalkSpeedCheat {
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            values: Mutex::new(Value::Object(Default::default())),
            original: Mutex::new(None),
        }
    }
}

impl Default for WalkSpeedCheat {
    fn default() -> Self {
        Self::new()
    }
}

impl ToggleCheat for WalkSpeedCheat {
    fn name(&self) -> &'static str {
        "walkspeed"
    }

    fn display_name(&self) -> &'static str {
        "WalkSpeed"
    }

    fn description(&self) -> &'static str {
        "Change your character's walk speed"
    }

    fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    fn values(&self) -> Value {
        self.values.lock().clone()
    }

    fn store_values(&self, values: &Value) {
        merge_values(&mut self.values.lock(), values.clone());
    }

    fn enable(&self, roblox: &Arc<Roblox>, values: Value) -> Result<(), String> {
        self.store_values(&values);

        let speed = self
            .values
            .lock()
            .get("speed")
            .and_then(Value::as_f64)
            .map(|speed| speed as f32)
            .unwrap_or(DEFAULT_SPEED)
            .max(0.0);

        let humanoid = roblox
            .get_local_humanoid()
            .ok_or_else(|| "Local humanoid not found".to_string())?;

        // update_values() re-applies this, so only remember the true original once.
        {
            let mut original = self.original.lock();
            if original.is_none() {
                *original = Some(humanoid.walk_speed());
            }
        }

        if !humanoid.set_walk_speed(speed) {
            return Err("Failed to write WalkSpeed".to_string());
        }

        self.enabled.store(true, Ordering::Release);
        Ok(())
    }

    fn disable(&self, roblox: &Arc<Roblox>) -> Result<(), String> {
        let humanoid = roblox.get_local_humanoid();
        let original = self.original.lock().take();

        if let (Some(humanoid), Some(original)) = (humanoid, original) {
            humanoid.set_walk_speed(original);
        }

        self.enabled.store(false, Ordering::Release);
        Ok(())
    }

    fn reset(&self) {
        self.enabled.store(false, Ordering::Release);
        *self.original.lock() = None;
    }
}
