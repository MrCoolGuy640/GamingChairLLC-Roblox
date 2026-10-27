//! Fling: tumble the character on random angular velocity, then stand it back
//! up. This is the good folder's `spin`, renamed in this app.
//!
//! The tumble itself is a RANDOM slot in the freezer: three floats around
//! `speed`, re-rolled every pass with random signs so it tumbles instead of
//! settling onto one axis. No Rust runs per tick.
//!
//! Toggling off releases the slot, then zeroes the spin and stands the
//! character back up, keeping whatever yaw it is facing at that moment. The
//! stand-up runs on its own thread (it can wait up to `settle_timeout`), and
//! Fly's teardown waits for it so the freeze is not released under a ragdoll.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};

use super::session::SharedSession;
use super::{boolean, number, text, TickedCheat, TickedMeta};
use crate::sdk::input::{key_down, vk_from_label};
use crate::sdk::roblox::camera::Camera;
use crate::sdk::roblox::math::{self, Mat3};
use crate::sdk::roblox::part::Part;
use crate::sdk::roblox::vector3::Vector3;
use crate::sdk::roblox::Roblox;

#[derive(Clone, Debug)]
struct FlingSettings {
    speed: f32,
    stop_frames: u32,
    settle_timeout: f64,
    stabilize_camera: bool,
    keybind: String,
}

impl FlingSettings {
    fn from_values(values: &Value) -> Self {
        Self {
            speed: number(values, "speed", 1000.0).max(0.0) as f32,
            stop_frames: number(values, "stop_frames", 10.0).max(1.0) as u32,
            settle_timeout: number(values, "settle_timeout", 15.0).max(0.0),
            stabilize_camera: boolean(values, "stabilize_camera", true),
            keybind: text(values, "keybind", "E"),
        }
    }
}

pub struct FlingCheat {
    meta: TickedMeta,
    shared: SharedSession,
    settings: Mutex<FlingSettings>,
    /// Angular-velocity freezer slot, 0 when not tumbling.
    slot: Mutex<u64>,
    /// Camera and subject saved for the duration of the tumble.
    camera: Mutex<Option<crate::sdk::roblox::Instance>>,
    saved_subject: Mutex<u64>,
    /// Character parts resolved when the tumble started, used as a fallback
    /// if the character cannot be enumerated during the stand-up.
    parts_on: Mutex<Vec<Part>>,
    yaw_on: Mutex<Option<f32>>,
}

impl FlingCheat {
    pub fn new(shared: SharedSession) -> Self {
        Self {
            meta: TickedMeta::with_values(default_values()),
            shared,
            settings: Mutex::new(FlingSettings {
                speed: 1000.0,
                stop_frames: 10,
                settle_timeout: 15.0,
                stabilize_camera: true,
                keybind: "E".to_string(),
            }),
            slot: Mutex::new(0),
            camera: Mutex::new(None),
            saved_subject: Mutex::new(0),
            parts_on: Mutex::new(Vec::new()),
            yaw_on: Mutex::new(None),
        }
    }

    fn release_slot(&self, roblox: &Arc<Roblox>) {
        let slot = std::mem::replace(&mut *self.slot.lock(), 0);
        if slot != 0 {
            roblox.context().freezer.unfreeze(slot);
        }
    }

    fn restore_camera(&self) {
        let saved = std::mem::replace(&mut *self.saved_subject.lock(), 0);
        let camera = self.camera.lock().take();
        restore_camera_subject(camera, saved);
    }

    fn steady_camera(&self, session: &Arc<super::session::FlySession>, root_address: u64) {
        let camera = session.camera_instance();
        let Some(camera) = camera else {
            return;
        };
        let camera = Camera::new(camera);

        let current = camera.camera_subject();
        if current != 0 && current != root_address {
            *self.saved_subject.lock() = current;
            camera.set_camera_subject(root_address);
            *self.camera.lock() = Some(camera.instance().clone());
        }
    }
}

impl Default for FlingCheat {
    fn default() -> Self {
        Self::new(SharedSession::new())
    }
}

impl TickedCheat for FlingCheat {
    fn name(&self) -> &'static str {
        "fling"
    }

    fn display_name(&self) -> &'static str {
        "Fling"
    }

    fn description(&self) -> &'static str {
        "Tumble your character on random angular velocity. Requires Fly."
    }

    fn meta(&self) -> &TickedMeta {
        &self.meta
    }

    fn tick_interval(&self) -> Option<f64> {
        // The tumble lives in the freezer. This interval only re-applies a
        // changed speed, so it can be lazy.
        Some(0.5)
    }

    fn on_start(&self, roblox: &Arc<Roblox>, values: &Value) -> Result<(), String> {
        let settings = FlingSettings::from_values(values);

        let Some(session) = self.shared.get() else {
            return Err("Fling needs Fly".to_string());
        };

        let root_address = session.root.instance().address();
        // Resolve the fallbacks while the character is calm, before the tumble
        // starts (the Python `enable` does the same, and its reads can fail
        // mid-spin).
        *self.yaw_on.lock() = session
            .root
            .rotation_matrix()
            .map(|rows| math::yaw_from_matrix(&rows));
        *self.parts_on.lock() = session.parts();

        // A previous stand-up (from a fast off/on) must not clear this run's
        // rotation claim when it eventually finishes.
        session.cancel_standup();
        session.set_rotation_claimed(true);

        if settings.stabilize_camera {
            self.steady_camera(&session, root_address);
        }

        let Some(slot) = session.root.angular_velocity_address() else {
            self.restore_camera();
            session.set_rotation_claimed(false);
            return Err("Fling: could not resolve the angular-velocity slot".to_string());
        };

        roblox.context().freezer.freeze_random(
            slot,
            12,
            settings.speed * 0.85,
            settings.speed * 1.15,
            true,
        );
        *self.slot.lock() = slot;
        *self.settings.lock() = settings.clone();

        log::info!(
            target: "GamingChairLLC.fling",
            "Fling on (speed {:.0}, stop {} frame(s), settle {:.1}s)",
            settings.speed,
            settings.stop_frames,
            settings.settle_timeout
        );

        Ok(())
    }

    fn on_tick(&self, _roblox: &Arc<Roblox>, _now: f64) {
        // The freezer owns the effect. Nothing to do per tick.
    }

    fn on_values(&self, _roblox: &Arc<Roblox>, values: &Value) {
        let settings = FlingSettings::from_values(values);
        let slot = *self.slot.lock();

        if slot != 0 {
            // Re-range the live slot so a speed change applies immediately.
            if let Some(session) = self.shared.get() {
                session.root.instance().ctx().freezer.update_random(
                    slot,
                    settings.speed * 0.85,
                    settings.speed * 1.15,
                    true,
                );
            }
        }

        *self.settings.lock() = settings;
    }

    fn on_stop(&self, roblox: &Arc<Roblox>) {
        self.release_slot(roblox);

        let Some(session) = self.shared.get() else {
            self.restore_camera();
            return;
        };

        let settings = self.settings.lock().clone();
        let root = Part::new(session.root.instance().clone());

        let yaw = session
            .root
            .rotation_matrix()
            .map(|rows| math::yaw_from_matrix(&rows))
            .or_else(|| *self.yaw_on.lock())
            .unwrap_or(0.0);
        let upright: Mat3 = math::rotation_matrix_from_yaw(yaw);

        let mut parts = std::mem::take(&mut *self.parts_on.lock());
        if parts.is_empty() {
            parts = session.parts();
        }

        let keybind = settings.keybind.clone();
        let vk = vk_from_label(&keybind);
        let generation = session.next_standup_generation();

        let camera = self.camera.lock().take();
        let saved_subject = std::mem::replace(&mut *self.saved_subject.lock(), 0);

        let parts = Arc::new(parts);
        let thread_parts = Arc::clone(&parts);
        let thread_root = Part::new(session.root.instance().clone());
        let thread_settings = settings.clone();
        let thread_roblox = Arc::clone(roblox);
        let thread_session = Arc::clone(&session);
        let thread_camera = camera.clone();
        let standup = std::thread::Builder::new()
            .name("fling-standup".into())
            .spawn(move || {
                stand_up(
                    &thread_roblox,
                    &thread_session,
                    &thread_root,
                    thread_parts.as_slice(),
                    &upright,
                    &thread_settings,
                    vk,
                    generation,
                );

                // Restore the camera before dropping the rotation claim: the
                // camera-facing writer has to see a settled character, exactly
                // like the Python `finally` ordering. A newer Fling cancels
                // this stand-up, and it must not touch the newer run's state.
                if thread_session.is_current_standup(generation) {
                    restore_camera_subject(thread_camera, saved_subject);
                    thread_session.finish_standup(generation);
                    log::info!(
                        target: "GamingChairLLC.fling",
                        "Fling off, stood up at yaw {:.1}",
                        yaw.to_degrees()
                    );
                }
            });

        match standup {
            Ok(handle) => session.set_standup_handle(
                handle,
                Duration::from_secs_f64(settings.settle_timeout),
            ),
            Err(err) => {
                // Could not get a thread: do the same work inline rather than
                // leaving the character spinning.
                log::warn!(
                    target: "GamingChairLLC.fling",
                    "Could not start the stand-up thread ({err}); standing up inline"
                );
                stand_up(
                    roblox,
                    &session,
                    &root,
                    parts.as_slice(),
                    &upright,
                    &settings,
                    vk,
                    generation,
                );

                if session.is_current_standup(generation) {
                    restore_camera_subject(camera, saved_subject);
                    session.finish_standup(generation);
                }
            }
        }
    }
}

fn restore_camera_subject(camera: Option<crate::sdk::roblox::Instance>, saved_subject: u64) {
    if saved_subject == 0 {
        return;
    }

    if let Some(camera) = camera {
        Camera::new(camera).set_camera_subject(saved_subject);
    }
}

fn stand_up(
    roblox: &Arc<Roblox>,
    session: &Arc<super::session::FlySession>,
    root: &Part,
    parts: &[Part],
    upright: &Mat3,
    settings: &FlingSettings,
    vk: Option<i32>,
    generation: u64,
) {
    let stop = Vector3::ZERO;
    let root_address = root.instance().address();

    let write_upright = || {
        // A fast off/on starts a new tumble before this thread is done. Once
        // superseded, stop writing: the new Fling owns the rotation now.
        if !session.is_current_standup(generation) {
            return;
        }

        for part in parts {
            part.write_angular_velocity(stop, false);
            // Linear too: a tumbling limb carries both and the solver feeds
            // either one back into the assembly. The root is the exception --
            // its linear velocity belongs to the Fly freeze.
            if part.instance().address() != root_address {
                part.write_velocity(stop, false);
            }
        }

        // Unconditional, like the Python: the root may not be in the
        // enumerated parts, but it is always the part that has to stand up.
        root.write_rotation_matrix(upright);
    };

    // One write lands mid-step; hold it across a few rendered frames.
    let timeout = Duration::from_secs_f64((settings.stop_frames as f64 / 20.0).max(0.5));
    let mut frames = roblox.hold_frames(settings.stop_frames, timeout, write_upright);

    // Then keep asserting until it stays. A hard tumble knocks the humanoid
    // into a fallen state, and a ragdolled character lies straight back down a
    // frame after one clean write.
    let mut stable = 0u32;
    let mut released = vk.map(|vk| !key_down(vk)).unwrap_or(true);
    let deadline = Instant::now() + Duration::from_secs_f64(settings.settle_timeout);

    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(30));

        if !session.is_current_standup(generation) {
            log::info!(target: "GamingChairLLC.fling", "Stand-up superseded");
            break;
        }

        if let Some(vk) = vk {
            // The settle window is long, so let the key cut it short.
            let down = key_down(vk);
            if released && down {
                log::info!(target: "GamingChairLLC.fling", "Stand-up interrupted");
                break;
            }
            released = released || !down;
        }

        if is_upright(session) {
            stable += 1;
            if stable >= 3 {
                break;
            }
        } else {
            stable = 0;
            frames += roblox.hold_frames(settings.stop_frames, timeout, write_upright);
        }
    }

    log::info!(
        target: "GamingChairLLC.fling",
        "Stand-up finished ({frames} frame(s) held, {} part(s) zeroed, {})",
        parts.len(),
        if stable >= 3 { "settled" } else { "still fighting it" }
    );
}

fn is_upright(session: &Arc<super::session::FlySession>) -> bool {
    // r1.y is cos(pitch): 1.0 upright, 0.0 flat on its back.
    let upright = session
        .root
        .rotation_matrix()
        .map(|rows| rows[1].y >= 0.999)
        .unwrap_or(false);

    let still = session
        .root
        .angular_velocity()
        .map(|av| av.x * av.x + av.y * av.y + av.z * av.z < 1.0)
        .unwrap_or(false);

    upright && still
}

pub fn default_values() -> Value {
    json!({
        "keybind": "E",
        "speed": 1000.0,
        "stop_frames": 10,
        "settle_timeout": 15.0,
        "stabilize_camera": true,
    })
}
