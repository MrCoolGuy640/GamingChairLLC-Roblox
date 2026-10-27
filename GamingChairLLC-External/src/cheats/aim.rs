//! Aim Assist: keep the camera pointed at the nearest other player.
//!
//! This is the good folder's `lock`, with a smoother hand-off. The original
//! wrote the exact look-at matrix every tick, which is snappy but visibly
//! steppy and fights the camera script. This version eases toward the target
//! with a quaternion slerp whose per-tick blend is `1 - exp(-dt / tau)`, then
//! hands the result to the freezer as a HOLD slot. The worker hammers the slot
//! at its own rate, which is what actually out-writes the camera script.
//!
//! Only the camera's ORIENTATION is taken. Its position is still the game's
//! own, so zoom, orbit and follow distance behave normally.
//!
//! Standalone: with Fly off it resolves its own camera and character. When a
//! Fly session exists it reuses what that session already resolved.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};

use super::session::SharedSession;
use super::{boolean, number, text, TickedCheat, TickedMeta};
use crate::sdk::roblox::camera::Camera;
use crate::sdk::roblox::instance::Instance;
use crate::sdk::roblox::math::{self, Mat3, Quaternion};
use crate::sdk::roblox::part::Part;
use crate::sdk::roblox::utils;
use crate::sdk::roblox::vector3::Vector3;
use crate::sdk::roblox::Roblox;

const RESOLVE_INTERVAL: Duration = Duration::from_millis(500);
const TICK_INTERVAL: f64 = 0.01;

#[derive(Clone, Debug)]
struct AimSettings {
    smoothing: f32,
    strength: f32,
    stickiness: f32,
    screen_mode: bool,
    radius: f32,
    aim_part: String,
}

impl AimSettings {
    fn from_values(values: &Value) -> Self {
        Self {
            smoothing: number(values, "smoothing", 0.08).max(0.0) as f32,
            strength: number(values, "strength", 1.0).clamp(0.0, 1.0) as f32,
            stickiness: number(values, "stickiness", 0.15).clamp(0.0, 0.95) as f32,
            screen_mode: boolean(values, "screen_mode", true),
            radius: number(values, "radius", 150.0).max(1.0) as f32,
            aim_part: text(values, "aim_part", "head"),
        }
    }
}

pub struct AimCheat {
    meta: TickedMeta,
    shared: SharedSession,
    settings: Mutex<AimSettings>,

    slot: Mutex<u64>,
    active: AtomicBool,
    /// The part we are currently aiming at, for the hysteresis check.
    target: Mutex<Option<u64>>,
    /// Our eased orientation. Reset whenever a target is lost or acquired.
    smoothed: Mutex<Option<Quaternion>>,
    last_tick: Mutex<Instant>,

    cached_camera: Mutex<Option<Instance>>,
    cached_camera_at: Mutex<Instant>,
    cached_players: Mutex<Option<Instance>>,
    cached_players_at: Mutex<Instant>,
}

impl AimCheat {
    pub fn new(shared: SharedSession) -> Self {
        Self {
            meta: TickedMeta::with_values(default_values()),
            shared,
            settings: Mutex::new(AimSettings {
                smoothing: 0.08,
                strength: 1.0,
                stickiness: 0.15,
                screen_mode: true,
                radius: 150.0,
                aim_part: "head".to_string(),
            }),
            slot: Mutex::new(0),
            active: AtomicBool::new(false),
            target: Mutex::new(None),
            smoothed: Mutex::new(None),
            last_tick: Mutex::new(Instant::now()),
            cached_camera: Mutex::new(None),
            cached_camera_at: Mutex::new(Instant::now() - RESOLVE_INTERVAL),
            cached_players: Mutex::new(None),
            cached_players_at: Mutex::new(Instant::now() - RESOLVE_INTERVAL),
        }
    }

    fn settings(&self) -> AimSettings {
        self.settings.lock().clone()
    }

    fn camera(&self, roblox: &Arc<Roblox>) -> Option<Camera> {
        if let Some(session) = self.shared.get() {
            if let Some(camera) = session.camera() {
                return Some(camera);
            }
        }

        let mut cached = self.cached_camera.lock();
        let mut at = self.cached_camera_at.lock();

        if cached.is_none() || at.elapsed() >= RESOLVE_INTERVAL {
            *cached = roblox.get_camera().and_then(|camera| {
                // Fail here, not in the hot path.
                camera.rotation_matrix()?;
                camera.position()?;
                Some(camera.instance().clone())
            });
            *at = Instant::now();
        }

        cached.clone().map(Camera::new)
    }

    fn players(&self, roblox: &Arc<Roblox>) -> Option<Instance> {
        let mut cached = self.cached_players.lock();
        let mut at = self.cached_players_at.lock();

        if cached.is_none() || at.elapsed() >= RESOLVE_INTERVAL {
            *cached = roblox.get_players();
            *at = Instant::now();
        }

        cached.clone()
    }

    fn arm(&self, roblox: &Arc<Roblox>, camera: &Camera) -> bool {
        let address = camera.rotation_address();
        if address == 0 {
            return false;
        }

        let rows = camera.rotation_matrix().unwrap_or_else(math::identity);
        let freezer = &roblox.context().freezer;
        freezer.freeze(address, math::to_bytes(&rows).to_vec());
        freezer.set_active(address, false);

        *self.slot.lock() = address;
        self.active.store(false, Ordering::Release);
        *self.target.lock() = None;
        *self.smoothed.lock() = None;
        true
    }

    fn release(&self, roblox: &Arc<Roblox>) {
        let address = std::mem::replace(&mut *self.slot.lock(), 0);
        if address == 0 {
            return;
        }

        let freezer = &roblox.context().freezer;
        freezer.set_active(address, false);
        freezer.unfreeze(address);
        self.active.store(false, Ordering::Release);
        *self.target.lock() = None;
        *self.smoothed.lock() = None;
    }

    /// No target this tick: hand the camera back to the game.
    ///
    /// Parking matters: the slot holds whatever matrix was written last, so
    /// leaving it armed with no target would freeze the camera pointing
    /// wherever the last target was.
    fn park(&self, roblox: &Arc<Roblox>) {
        *self.target.lock() = None;
        *self.smoothed.lock() = None;

        if !self.active.swap(false, Ordering::AcqRel) {
            return;
        }

        let address = *self.slot.lock();
        if address != 0 {
            roblox.context().freezer.set_active(address, false);
        }
    }
}

impl Default for AimCheat {
    fn default() -> Self {
        Self::new(SharedSession::new())
    }
}

impl TickedCheat for AimCheat {
    fn name(&self) -> &'static str {
        "aim"
    }

    fn display_name(&self) -> &'static str {
        "Aim Assist"
    }

    fn description(&self) -> &'static str {
        "Smoothly keep the camera aimed at the nearest other player."
    }

    fn meta(&self) -> &TickedMeta {
        &self.meta
    }

    fn tick_interval(&self) -> Option<f64> {
        Some(TICK_INTERVAL)
    }

    fn on_start(&self, roblox: &Arc<Roblox>, values: &Value) -> Result<(), String> {
        *self.settings.lock() = AimSettings::from_values(values);
        *self.last_tick.lock() = Instant::now();

        let Some(camera) = self.camera(roblox) else {
            return Err("Aim Assist needs a camera".to_string());
        };

        if !self.arm(roblox, &camera) {
            return Err("Aim Assist could not arm the camera slot".to_string());
        }

        log::info!(
            target: "GamingChairLLC.aim",
            "Aim Assist on ({})",
            self.settings().aim_part
        );
        Ok(())
    }

    fn on_tick(&self, roblox: &Arc<Roblox>, _now: f64) {
        let Some(camera) = self.camera(roblox) else {
            self.park(roblox);
            return;
        };

        // The Camera instance can be replaced (respawn, a place swapping
        // CurrentCamera), which moves the matrix we are holding. Re-arm on the
        // new address rather than writing forever to a dead slot.
        let address = camera.rotation_address();
        if address == 0 || address != *self.slot.lock() {
            self.release(roblox);
            if !self.arm(roblox, &camera) {
                return;
            }
        }

        let Some(rows) = camera.rotation_matrix() else {
            self.park(roblox);
            return;
        };
        let Some(eye) = camera.position() else {
            self.park(roblox);
            return;
        };

        let settings = self.settings();
        let players = match self.players(roblox) {
            Some(players) => players,
            None => {
                self.park(roblox);
                return;
            }
        };

        let origin = self
            .shared
            .get()
            .and_then(|session| session.root.position())
            .unwrap_or(eye);

        // Screen mode needs the projection to rank; world mode does not.
        let projection = if settings.screen_mode {
            match (camera.field_of_view(), camera.viewport_size()) {
                (Some(fov), Some(size)) => Some((fov, size)),
                _ => None,
            }
        } else {
            None
        };

        if settings.screen_mode && projection.is_none() {
            self.park(roblox);
            return;
        }

        let candidates = self.candidates(roblox, &players, &settings);
        let Some((target_address, target_position)) =
            self.pick(&candidates, &rows, eye, origin, projection, &settings)
        else {
            self.park(roblox);
            return;
        };

        let Some(desired) = math::look_at_rows(eye, target_position) else {
            self.park(roblox);
            return;
        };

        let now = Instant::now();
        let mut last = self.last_tick.lock();
        let dt = now
            .duration_since(*last)
            .as_secs_f32()
            .clamp(0.0001, 0.1);
        *last = now;
        drop(last);

        let mut smoothed = self.smoothed.lock();
        let target_changed = *self.target.lock() != Some(target_address);
        if target_changed {
            *self.target.lock() = Some(target_address);
            // Start from where the camera actually is, so acquiring a new
            // target does not snap to the old aim.
            *smoothed = Some(math::quaternion_from_matrix(&rows));
        }

        let current = smoothed.unwrap_or_else(|| math::quaternion_from_matrix(&rows));
        let desired = math::quaternion_from_matrix(&desired);

        let blend = if settings.smoothing <= 0.0 {
            1.0
        } else {
            1.0 - (-dt / settings.smoothing).exp()
        };
        let t = (blend * settings.strength).clamp(0.0, 1.0);
        let next = math::slerp(current, desired, t);
        *smoothed = Some(next);

        let matrix: Mat3 = math::matrix_from_quaternion(next);
        let slot = *self.slot.lock();
        if slot == 0 {
            return;
        }

        let freezer = &roblox.context().freezer;
        freezer.update(slot, &math::to_bytes(&matrix));
        freezer.set_active(slot, true);
        self.active.store(true, Ordering::Release);
    }

    fn on_values(&self, _roblox: &Arc<Roblox>, values: &Value) {
        let settings = AimSettings::from_values(values);
        let part_changed = self.settings.lock().aim_part != settings.aim_part;

        *self.settings.lock() = settings;

        if part_changed {
            *self.target.lock() = None;
            *self.smoothed.lock() = None;
        }
    }

    fn on_stop(&self, roblox: &Arc<Roblox>) {
        self.release(roblox);
        log::info!(target: "GamingChairLLC.aim", "Aim Assist off");
    }
}

impl AimCheat {
    /// `(part address, position)` for every other living player who has
    /// spawned. A character with no Humanoid, or whose health cannot be read,
    /// counts as alive.
    fn candidates(
        &self,
        roblox: &Arc<Roblox>,
        players: &Instance,
        settings: &AimSettings,
    ) -> Vec<(u64, Vector3)> {
        let local = players.ctx().memory.read_ptr(
            players
                .address()
                .wrapping_add(players.ctx().offsets.player.local_player),
        );

        let Ok(datamodel) = roblox.get_datamodel() else {
            return Vec::new();
        };
        let workspace = datamodel.find_first_child_of_name("Workspace");

        let mut out = Vec::new();
        for child in players.get_children() {
            if child.address() == local {
                continue;
            }

            let player = crate::sdk::roblox::player::Player::new(child);
            let Some(character) =
                utils::get_character(&datamodel, &player, workspace.as_ref())
            else {
                continue;
            };
            let Some(part) = aim_part(&character, &settings.aim_part) else {
                continue;
            };

            if let Some(health) = utils::get_humanoid_health(&character) {
                if health <= 0.0 {
                    continue;
                }
            }

            if let Some(position) = part.position() {
                if position.is_finite() {
                    out.push((part.instance().address(), position));
                }
            }
        }

        out
    }

    #[allow(clippy::too_many_arguments)]
    fn pick(
        &self,
        candidates: &[(u64, Vector3)],
        rows: &Mat3,
        eye: Vector3,
        origin: Vector3,
        projection: Option<(f32, (f32, f32))>,
        settings: &AimSettings,
    ) -> Option<(u64, Vector3)> {
        let current_address = *self.target.lock();
        let mut best: Option<(u64, Vector3, f32)> = None;
        let mut current: Option<(u64, Vector3, f32)> = None;

        for (address, position) in candidates {
            let distance = if settings.screen_mode {
                let (fov, size) = projection?;
                let Some((distance, _depth)) = math::project(rows, eye, fov, size, *position)
                else {
                    continue;
                };
                if distance > settings.radius {
                    continue;
                }
                distance
            } else {
                origin.distance(*position)
            };

            if current_address == Some(*address) {
                current = Some((*address, *position, distance));
            }

            if best.map(|(_, _, best_d)| distance < best_d).unwrap_or(true) {
                best = Some((*address, *position, distance));
            }
        }

        let best = best?;

        // Stay on the current target unless the new one is meaningfully
        // closer, so two players at nearly equal range do not flicker.
        if let Some((current_address, current_position, current_d)) = current {
            if best.2 > current_d * (1.0 - settings.stickiness) {
                return Some((current_address, current_position));
            }
        }

        Some((best.0, best.1))
    }
}

fn aim_part(character: &Instance, part: &str) -> Option<Part> {
    let head = if part.eq_ignore_ascii_case("head") {
        character.find_first_child_of_name("Head")
    } else {
        None
    };

    // Every rig has a HumanoidRootPart; a custom or headless avatar may not
    // have a Head, so fall back rather than dropping that player.
    head.or_else(|| character.find_first_child_of_name("HumanoidRootPart"))
        .map(Part::new)
}

pub fn default_values() -> Value {
    json!({
        "keybind": "C",
        "smoothing": 0.08,
        "strength": 1.0,
        "stickiness": 0.15,
        "screen_mode": true,
        "radius": 150.0,
        "aim_part": "head",
    })
}
