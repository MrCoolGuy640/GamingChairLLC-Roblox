//! Fly: freeze the character in place and drive it with WASD / Space / Ctrl.
//!
//! All of the state lives in `FlySession`; this cheat is the lifecycle around
//! it: parse values, start a session, step movement at 100 Hz, and tear the
//! session down. Fling and Sweep resolve the same session through `SharedSession`
//! so they cannot run without it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};

use super::session::{FlySession, MovementInput, MovementSettings, SharedSession};
use super::{boolean, number, TickedCheat, TickedMeta};
use crate::sdk::input::key_down;
use crate::sdk::process;
use crate::sdk::roblox::Roblox;

// GetAsyncKeyState virtual-key codes for the movement keys.
const VK_W: i32 = 0x57;
const VK_A: i32 = 0x41;
const VK_S: i32 = 0x53;
const VK_D: i32 = 0x44;
const VK_SPACE: i32 = 0x20;
const VK_CTRL: i32 = 0x11;
const VK_Q: i32 = 0x51;

const CAMERA_REFRESH: Duration = Duration::from_secs(1);

#[derive(Clone, Debug)]
pub struct FlySettings {
    pub speed: f32,
    pub boost_power: f32,
    pub floor_margin: f32,
    pub face_camera: bool,
}

impl FlySettings {
    fn from_values(values: &Value) -> Self {
        Self {
            speed: number(values, "speed", 1.0).max(0.0) as f32,
            boost_power: number(values, "boost_power", 4.0).max(0.0) as f32,
            floor_margin: number(values, "floor_margin", 10.0).max(0.0) as f32,
            face_camera: boolean(values, "face_camera", true),
        }
    }

    fn movement(&self) -> MovementSettings {
        MovementSettings {
            speed: self.speed,
            boost_power: self.boost_power,
        }
    }
}

pub struct FlyCheat {
    meta: TickedMeta,
    shared: SharedSession,
    settings: Mutex<FlySettings>,
    last_camera_refresh: Mutex<Instant>,
}

impl FlyCheat {
    pub fn new(shared: SharedSession) -> Self {
        Self {
            meta: TickedMeta::with_values(default_values()),
            shared,
            settings: Mutex::new(FlySettings {
                speed: 1.0,
                boost_power: 4.0,
                floor_margin: 10.0,
                face_camera: true,
            }),
            last_camera_refresh: Mutex::new(Instant::now()),
        }
    }
}

impl Default for FlyCheat {
    fn default() -> Self {
        Self::new(SharedSession::new())
    }
}

impl TickedCheat for FlyCheat {
    fn name(&self) -> &'static str {
        "fly"
    }

    fn display_name(&self) -> &'static str {
        "Fly"
    }

    fn description(&self) -> &'static str {
        "Fly with WASD, Space and Ctrl. Hold Q to accelerate."
    }

    fn meta(&self) -> &TickedMeta {
        &self.meta
    }

    fn tick_interval(&self) -> Option<f64> {
        // 100 Hz, matching the Python movement loop's fixed step. The step is
        // per-tick, not per-second, so this interval *is* the fly speed.
        Some(0.01)
    }

    fn on_start(&self, roblox: &Arc<Roblox>, values: &Value) -> Result<(), String> {
        let settings = FlySettings::from_values(values);
        let session = FlySession::start(roblox, settings.floor_margin, settings.face_camera)?;

        log::info!(
            target: "GamingChairLLC.fly",
            "Fly on (floor {:.1}, face_camera {})",
            session.min_y(),
            settings.face_camera
        );

        *self.settings.lock() = settings;
        *self.last_camera_refresh.lock() = Instant::now();
        self.shared.set(Some(session));
        Ok(())
    }

    fn on_tick(&self, roblox: &Arc<Roblox>, _now: f64) {
        let Some(session) = self.shared.get() else {
            return;
        };

        let refresh_camera = {
            let mut last = self.last_camera_refresh.lock();
            let due = last.elapsed() >= CAMERA_REFRESH;
            if due {
                *last = Instant::now();
            }
            due
        };
        if refresh_camera {
            session.refresh_camera();
        }

        let focused = process::foreground_pid() == Some(roblox.pid()) && !roblox.is_chat_focused();

        let input = MovementInput {
            forward: axis(VK_W, VK_S),
            right: axis(VK_A, VK_D),
            up: axis(VK_SPACE, VK_CTRL),
            boost: key_down(VK_Q),
        };

        let settings = self.settings.lock().movement();
        session.step_movement(input, settings, focused, "fly");
    }

    fn on_values(&self, _roblox: &Arc<Roblox>, values: &Value) {
        let settings = FlySettings::from_values(values);

        if let Some(session) = self.shared.get() {
            session.set_face_camera(settings.face_camera);
            session.set_safe_margin(settings.floor_margin);
        }

        *self.settings.lock() = settings;
    }

    fn on_stop(&self, _roblox: &Arc<Roblox>) {
        if let Some(session) = self.shared.get() {
            // Fling stands the character up on its own thread; keep the freeze
            // (and the session) alive until it has finished.
            session.wait_for_standup();
            session.stop();
        }

        self.shared.clear();
        log::info!(target: "GamingChairLLC.fly", "Fly off");
    }
}

fn axis(positive: i32, negative: i32) -> f32 {
    let mut value = 0.0;
    if key_down(positive) {
        value += 1.0;
    }
    if key_down(negative) {
        value -= 1.0;
    }
    value
}

pub fn default_values() -> Value {
    json!({
        "keybind": "V",
        "speed": 1.0,
        "boost_power": 4.0,
        "floor_margin": 10.0,
        "face_camera": true,
    })
}
