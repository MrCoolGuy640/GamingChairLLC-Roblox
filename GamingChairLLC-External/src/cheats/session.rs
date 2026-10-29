//! The fly session: the character, the pinned position, the camera, and the
//! floor. Fly, Fling, Sweep and Aim Assist all drive or borrow one of these.
//!
//! The session owns exactly what the Python `FlySession` owns, and nothing
//! else:
//!
//!   * the root part, the character and the camera
//!   * `abs_pos`, the position the freezer is pinned to, clamped to the floor
//!   * the claim on `abs_pos`, so only one feature drives it at a time
//!   * the camera-facing COPY slot, and the rotation claim Fling takes over
//!
//! It also owns the movement step, so Fly is a thin lifecycle wrapper around
//! it. The math is the Python `position_transformation` floored to yaw (the
//! default there), which is identical while pitch is 0 and keeps WASD level.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::sdk::roblox::camera::Camera;
use crate::sdk::roblox::instance::Instance;
use crate::sdk::roblox::math;
use crate::sdk::roblox::part::Part;
use crate::sdk::roblox::utils;
use crate::sdk::roblox::vector3::Vector3;
use crate::sdk::roblox::Roblox;

/// The hard-coded floor: this is the value the good folder confirmed by
/// dropping a character through it. A place that destroys parts higher up only
/// ever raises the floor.
pub const DEFAULT_KILL_Y: f32 = -500.0;
pub const DEFAULT_SAFE_MARGIN: f32 = 10.0;
pub const DEFAULT_VELOCITY_Y: f32 = 16.0;

/// How long past `settle_timeout` Fly keeps waiting for a stand-up. The
/// stand-up itself stops at that timeout; this only covers the bookkeeping it
/// does afterwards (camera restore, claim release).
const STANDUP_JOIN_MARGIN: Duration = Duration::from_secs(2);

struct FaceSlot {
    dst: u64,
    src: u64,
    armed: bool,
    active: bool,
}

/// Input for one movement tick. Each field is the signed sum of its keys:
/// `forward` is W (+1) and S (-1), `right` is A (+1) and D (-1), `up` is
/// Space (+1) and Ctrl (-1).
#[derive(Clone, Copy, Debug, Default)]
pub struct MovementInput {
    pub forward: f32,
    pub right: f32,
    pub up: f32,
    pub boost: bool,
}

impl MovementInput {
    pub fn is_moving(&self) -> bool {
        self.forward != 0.0 || self.right != 0.0 || self.up != 0.0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MovementSettings {
    pub speed: f32,
    pub boost_power: f32,
}

impl Default for MovementSettings {
    fn default() -> Self {
        Self {
            speed: 1.0,
            boost_power: 4.0,
        }
    }
}

pub struct FlySession {
    roblox: Arc<Roblox>,
    pub root: Part,
    pub character: Instance,

    camera: Mutex<Option<Instance>>,
    face: Mutex<FaceSlot>,

    abs_pos: Mutex<Option<Vector3>>,
    position_owner: Mutex<Option<&'static str>>,

    kill_y: f32,
    min_y: Mutex<f32>,
    /// When off, `abs_pos` is never clamped and the player may fall into the
    /// void. The clamp itself is unchanged; this only decides whether it runs.
    no_void: AtomicBool,
    face_camera: AtomicBool,
    /// Fling owns the character's rotation (tumbling and standing up), so the
    /// camera-facing COPY slot stands down for the whole of it.
    rotation_claimed: AtomicBool,

    standup_gen: AtomicU64,
    standup_done: AtomicBool,
    standup: Mutex<Option<JoinHandle<()>>>,
    standup_deadline: Mutex<Instant>,

    last_clamp_note: Mutex<Instant>,
    boost_start: Mutex<Instant>,
}

impl FlySession {
    /// Resolve the character, clamp the floor, freeze position and velocity,
    /// and arm the camera-facing slot.
    pub fn start(
        roblox: &Arc<Roblox>,
        safe_margin: f32,
        face_camera: bool,
        no_void: bool,
    ) -> Result<Arc<Self>, String> {
        let root = roblox
            .get_local_root_part()
            .ok_or_else(|| "No character found".to_string())?;
        let character = roblox
            .get_local_character()
            .ok_or_else(|| "No character found".to_string())?;

        let safe_margin = if safe_margin.is_finite() {
            safe_margin.max(0.0)
        } else {
            DEFAULT_SAFE_MARGIN
        };

        // Only ever raises: the hard-coded floor is the guarantee.
        let mut kill_y = DEFAULT_KILL_Y;
        let mut min_y = kill_y + safe_margin;
        if let Some(live) = roblox.floor_height() {
            if live > -1.0e6 && live < 1.0e6 {
                let floor = live + safe_margin;
                if floor > min_y {
                    kill_y = live;
                    min_y = floor;
                }
            }
        }

        let position = root
            .position()
            .ok_or_else(|| "Could not read the root position".to_string())?;

        let camera = roblox.get_camera().and_then(|camera| {
            // Fail now, not a hundred times a second: steering needs a look
            // vector, so a camera without one is treated as unavailable.
            camera.look_vector()?;
            Some(camera.instance().clone())
        });

        let session = Arc::new(Self {
            roblox: Arc::clone(roblox),
            root,
            character,
            camera: Mutex::new(camera),
            face: Mutex::new(FaceSlot {
                dst: 0,
                src: 0,
                armed: false,
                active: false,
            }),
            abs_pos: Mutex::new(None),
            position_owner: Mutex::new(None),
            kill_y,
            min_y: Mutex::new(min_y),
            no_void: AtomicBool::new(no_void),
            face_camera: AtomicBool::new(face_camera),
            rotation_claimed: AtomicBool::new(false),
            standup_gen: AtomicU64::new(0),
            standup_done: AtomicBool::new(true),
            standup: Mutex::new(None),
            standup_deadline: Mutex::new(Instant::now()),
            last_clamp_note: Mutex::new(Instant::now() - Duration::from_secs(10)),
            boost_start: Mutex::new(Instant::now()),
        });

        session.set_abs_pos(Some(position));
        let position = session.abs_pos().unwrap_or(position);

        session.root.write_position(position, true);
        session
            .root
            .write_velocity(Vector3::new(0.0, DEFAULT_VELOCITY_Y, 0.0), true);

        session.arm_face();
        session.update_face_live();

        Ok(session)
    }

    pub fn min_y(&self) -> f32 {
        *self.min_y.lock()
    }

    /// Update the safe margin at runtime. The floor never lowers below the
    /// hard-coded value; this only ever moves the clamp up or back down to
    /// it.
    pub fn set_safe_margin(&self, margin: f32) {
        let margin = if margin.is_finite() {
            margin.max(0.0)
        } else {
            DEFAULT_SAFE_MARGIN
        };

        let mut min_y = self.min_y.lock();
        *min_y = (self.kill_y + margin).max(DEFAULT_KILL_Y + margin);
        let floor = *min_y;
        drop(min_y);

        let current = self.abs_pos();
        if let Some(current) = current {
            if current.y < floor {
                self.set_abs_pos(Some(current));
            }
        }
    }

    /// Turn the floor clamp on or off. It takes effect on the next position
    /// write, so switching it back on never yanks the player upwards.
    pub fn set_no_void(&self, enabled: bool) {
        self.no_void.store(enabled, Ordering::Release);
    }

    // -- the pinned position --------------------------------------------

    pub fn abs_pos(&self) -> Option<Vector3> {
        *self.abs_pos.lock()
    }

    /// Every position written into the session passes through here, so a
    /// feature cannot forget to clamp and Sweep landing on a player who is
    /// themselves below the floor is covered for free. With **No Void** off the
    /// clamp is skipped entirely and the player may drop into the void.
    pub fn set_abs_pos(&self, value: Option<Vector3>) {
        if !self.no_void.load(Ordering::Acquire) {
            *self.abs_pos.lock() = value;
            return;
        }

        let min_y = *self.min_y.lock();
        let value = match value {
            Some(value) if value.is_finite() && value.y < min_y => {
                let mut last = self.last_clamp_note.lock();
                if last.elapsed() >= Duration::from_secs(1) {
                    *last = Instant::now();
                    log::debug!(
                        target: "GamingChairLLC.fly",
                        "Floor: clamped y {:.1} -> {:.1} (kill plane {:.1})",
                        value.y,
                        min_y,
                        self.kill_y
                    );
                }
                Some(Vector3::new(value.x, min_y, value.z))
            }
            other => other,
        };

        *self.abs_pos.lock() = value;
    }

    /// Re-assert the frozen position. Called every movement tick.
    pub fn pin(&self) {
        if let Some(position) = self.abs_pos() {
            self.root.update_frozen_position(position);
        }
    }

    pub fn refresh_velocity(&self) {
        // The freezer reads the newest payload on its next pass.
        self.root
            .update_frozen_velocity(Vector3::new(0.0, DEFAULT_VELOCITY_Y, 0.0));
    }

    // -- who may drive it -----------------------------------------------

    pub fn drives_position(&self, owner: &'static str) -> bool {
        let current = *self.position_owner.lock();
        current.is_none() || current == Some(owner)
    }

    /// Claim the position channel. False when another feature already owns it.
    pub fn claim_position(&self, owner: &'static str) -> bool {
        let mut current = self.position_owner.lock();
        if current.is_some() {
            return false;
        }
        *current = Some(owner);
        true
    }

    pub fn release_position(&self, owner: &'static str) {
        let mut current = self.position_owner.lock();
        if *current == Some(owner) {
            *current = None;
        }
    }

    // -- the camera ------------------------------------------------------

    pub fn camera_instance(&self) -> Option<Instance> {
        self.camera.lock().clone()
    }

    pub fn camera(&self) -> Option<Camera> {
        self.camera_instance().map(Camera::new)
    }

    pub fn camera_look_yaw(&self) -> Option<f32> {
        self.camera()
            .and_then(|camera| camera.look_vector())
            .map(math::yaw_from_look)
    }

    /// Point the session at a new camera instance (a respawn can replace it).
    pub fn set_camera(&self, camera: Option<Instance>) {
        *self.camera.lock() = camera;
        self.arm_face();
        self.update_face_live();
    }

    /// Re-resolve `Workspace.CurrentCamera`; cheap enough for a slow tick.
    pub fn refresh_camera(&self) {
        let camera = self.roblox.get_camera().and_then(|camera| {
            camera.look_vector()?;
            Some(camera.instance().clone())
        });

        match (camera, self.camera_instance()) {
            (Some(new), Some(old)) if new.address() == old.address() => {}
            (new, _) => self.set_camera(new),
        }
    }

    pub fn set_face_camera(&self, enabled: bool) {
        self.face_camera.store(enabled, Ordering::Release);
        self.update_face_live();
    }

    // -- the camera-facing COPY slot ------------------------------------

    fn arm_face(&self) {
        let Some(camera) = self.camera_instance() else {
            return;
        };
        let src = Camera::new(camera).rotation_address();
        let Some(dst) = self.root.rotation_address() else {
            return;
        };
        if src == 0 || dst == 0 {
            return;
        }

        let mut face = self.face.lock();
        if face.armed && face.dst == dst && face.src == src {
            return;
        }

        let freezer = &self.root.instance().ctx().freezer;
        if face.armed {
            freezer.unfreeze(face.dst);
        }

        freezer.freeze_copy(dst, src, 36);
        freezer.set_active(dst, false);
        *face = FaceSlot {
            dst,
            src,
            armed: true,
            active: false,
        };
    }

    /// Decide whether the camera-facing copy is live right now.
    pub fn update_face_live(&self) {
        self.arm_face();

        let wanted = self.face_camera.load(Ordering::Acquire)
            && !self.rotation_claimed.load(Ordering::Acquire);

        let mut face = self.face.lock();
        if !face.armed || face.active == wanted {
            return;
        }

        self.root
            .instance()
            .ctx()
            .freezer
            .set_active(face.dst, wanted);
        face.active = wanted;
    }

    // -- fling rotation claim -------------------------------------------

    pub fn set_rotation_claimed(&self, claimed: bool) {
        self.rotation_claimed.store(claimed, Ordering::Release);
        self.update_face_live();
    }

    /// Begin a stand-up. Returns the generation the worker must report back
    /// with, so a stale worker cannot clear a newer Fling's claim.
    pub fn next_standup_generation(&self) -> u64 {
        self.standup_done.store(false, Ordering::Release);
        self.standup_gen.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn is_current_standup(&self, generation: u64) -> bool {
        self.standup_gen.load(Ordering::Acquire) == generation
    }

    pub fn finish_standup(&self, generation: u64) -> bool {
        if !self.is_current_standup(generation) {
            return false;
        }

        self.rotation_claimed.store(false, Ordering::Release);
        self.update_face_live();
        self.standup_done.store(true, Ordering::Release);
        true
    }

    /// A new Fling activation cancels any previous stand-up bookkeeping.
    pub fn cancel_standup(&self) {
        self.standup_gen.fetch_add(1, Ordering::AcqRel);
        self.standup_done.store(true, Ordering::Release);
    }

    /// Supersede and detach a stand-up without joining it.
    ///
    /// Death / leaving: the character the stand-up would write to is gone, so
    /// blocking the loop until `settle_timeout` would only delay the resume.
    /// The detached worker checks `is_current_standup` and stops writing.
    pub fn abandon_standup(&self) {
        self.cancel_standup();
        let _ = self.standup.lock().take();
    }

    pub fn set_standup_handle(&self, handle: JoinHandle<()>, settle: Duration) {
        *self.standup_deadline.lock() = Instant::now() + settle + STANDUP_JOIN_MARGIN;
        *self.standup.lock() = Some(handle);
    }

    /// Wait for the character to finish standing up before the freeze is
    /// released.
    ///
    /// The Python `spin.stop()` blocks its caller for the whole stand-up and
    /// only then lets `session.stop()` unfreeze; releasing position and
    /// velocity while the character is still ragdolled is what launches it.
    /// The bound is the same `settle_timeout` that ends the stand-up itself.
    pub fn wait_for_standup(&self) {
        let deadline = *self.standup_deadline.lock();
        while !self.standup_done.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }

        if self.standup_done.load(Ordering::Acquire) {
            if let Some(handle) = self.standup.lock().take() {
                let _ = handle.join();
            }
        }
    }

    // -- movement --------------------------------------------------------

    /// One 100 Hz movement step, plus the re-pin and the velocity refresh that
    /// happen whether or not anything moved.
    pub fn step_movement(
        &self,
        input: MovementInput,
        settings: MovementSettings,
        focused: bool,
        owner: &'static str,
    ) {
        let moving = input.is_moving();

        if moving && focused && self.drives_position(owner) {
            let yaw = self
                .camera_look_yaw()
                .or_else(|| {
                    self.root
                        .rotation_matrix()
                        .map(|rows| math::yaw_from_matrix(&rows))
                })
                .unwrap_or(0.0);

            let forward = math::forward(yaw);
            let right = math::right(yaw);

            let mut offset = Vector3::new(
                right.x * input.right + forward.x * input.forward,
                input.up,
                right.z * input.right + forward.z * input.forward,
            );

            if offset.magnitude() > 0.0 {
                offset = offset.normalize();
            }

            let speed = settings.speed.max(0.0);
            if speed != 1.0 {
                offset = offset * speed;
            }

            let boost = if input.boost {
                let start = *self.boost_start.lock();
                (1.0 + start.elapsed().as_secs_f32()).powf(settings.boost_power.max(0.0))
            } else {
                // Python only resets this timestamp while moving without the
                // modifier, which is exactly this branch.
                *self.boost_start.lock() = Instant::now();
                1.0
            };

            if boost != 1.0 {
                offset = offset * boost;
            }

            if let Some(current) = self.abs_pos() {
                self.set_abs_pos(Some(Vector3::new(
                    current.x + offset.x,
                    current.y + offset.y,
                    current.z + offset.z,
                )));
            }
        }

        self.pin();
        self.refresh_velocity();
    }

    // -- lifecycle -------------------------------------------------------

    pub fn stop(&self) {
        self.root.unfreeze_position();
        self.root.unfreeze_velocity();

        {
            let mut face = self.face.lock();
            if face.armed {
                let freezer = &self.root.instance().ctx().freezer;
                freezer.set_active(face.dst, false);
                freezer.unfreeze(face.dst);
                *face = FaceSlot {
                    dst: 0,
                    src: 0,
                    armed: false,
                    active: false,
                };
            }
        }

        *self.abs_pos.lock() = None;
        *self.position_owner.lock() = None;
    }

    pub fn parts(&self) -> Vec<Part> {
        utils::character_parts(&self.character, Some(&self.root))
    }
}

/// The handle every cheat uses to find the live session. It is empty whenever
/// Fly is off, which is how Fling and Sweep know they cannot run.
#[derive(Clone, Default)]
pub struct SharedSession(Arc<Mutex<Option<Arc<FlySession>>>>);

impl SharedSession {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self) -> Option<Arc<FlySession>> {
        self.0.lock().clone()
    }

    pub fn set(&self, session: Option<Arc<FlySession>>) {
        *self.0.lock() = session;
    }

    pub fn clear(&self) {
        self.set(None);
    }
}
