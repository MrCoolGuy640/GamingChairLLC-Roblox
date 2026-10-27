//! Sweep: pin yourself to every other player in turn, then back where you
//! started. This is the good folder's `tour`, renamed in this app.
//!
//! It moves `session.abs_pos` rather than writing the root part directly: the
//! movement loop re-pins the frozen position to `abs_pos` every tick, so a
//! direct write would be undone inside 10 ms. It also *claims* the position
//! channel for the duration, so WASD stops writing `abs_pos` instead of
//! quietly fighting it.
//!
//! The teleport loop runs on its own thread so a 0.5 s dwell per player never
//! blocks the backend loop. `take_finished` lets that thread switch the cheat
//! back off when it runs out of players.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};

use super::session::{FlySession, SharedSession};
use super::{number, text, TickedCheat, TickedMeta};
use crate::sdk::input::{key_down, vk_from_label};
use crate::sdk::roblox::part::Part;
use crate::sdk::roblox::utils;
use crate::sdk::roblox::Roblox;

const PIN_TICK: Duration = Duration::from_millis(10);
const ABORT_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Debug)]
struct SweepSettings {
    dwell: f64,
    keybind: String,
}

impl SweepSettings {
    fn from_values(values: &Value) -> Self {
        Self {
            dwell: number(values, "dwell", 0.5).max(0.0),
            keybind: text(values, "keybind", "R"),
        }
    }
}

pub struct SweepCheat {
    meta: TickedMeta,
    shared: SharedSession,
    settings: Mutex<SweepSettings>,
    abort: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl SweepCheat {
    pub fn new(shared: SharedSession) -> Self {
        Self {
            meta: TickedMeta::with_values(default_values()),
            shared,
            settings: Mutex::new(SweepSettings {
                dwell: 0.5,
                keybind: "R".to_string(),
            }),
            abort: Arc::new(AtomicBool::new(false)),
            done: Arc::new(AtomicBool::new(true)),
            finished: Arc::new(AtomicBool::new(false)),
            worker: Mutex::new(None),
        }
    }
}

impl Default for SweepCheat {
    fn default() -> Self {
        Self::new(SharedSession::new())
    }
}

impl TickedCheat for SweepCheat {
    fn name(&self) -> &'static str {
        "sweep"
    }

    fn display_name(&self) -> &'static str {
        "Sweep"
    }

    fn description(&self) -> &'static str {
        "Teleport to every other player in turn, then back to where you started."
    }

    fn meta(&self) -> &TickedMeta {
        &self.meta
    }

    fn tick_interval(&self) -> Option<f64> {
        // Possession is checked once the worker finishes; nothing ticks while
        // it is running.
        Some(0.1)
    }

    fn on_start(&self, roblox: &Arc<Roblox>, values: &Value) -> Result<(), String> {
        let settings = SweepSettings::from_values(values);

        let Some(session) = self.shared.get() else {
            return Err("Sweep needs Fly".to_string());
        };

        self.abort.store(false, Ordering::Release);
        self.done.store(false, Ordering::Release);
        self.finished.store(false, Ordering::Release);
        *self.settings.lock() = settings.clone();

        let abort = Arc::clone(&self.abort);
        let done = Arc::clone(&self.done);
        let finished = Arc::clone(&self.finished);
        let shared = self.shared.clone();
        let thread_roblox = Arc::clone(roblox);
        let thread_session = Arc::clone(&session);
        let vk = vk_from_label(&settings.keybind);

        let handle = std::thread::Builder::new()
            .name("sweep".into())
            .spawn(move || {
                run(
                    &thread_roblox,
                    &thread_session,
                    &shared,
                    settings.dwell,
                    vk,
                    &abort,
                    &done,
                );

                // One-shot: let the backend turn the switch back off, even
                // when the run ended early because there was nobody to visit.
                finished.store(true, Ordering::Release);
            })
            .map_err(|err| {
                self.done.store(true, Ordering::Release);
                format!("Could not start the Sweep thread: {err}")
            })?;

        *self.worker.lock() = Some(handle);
        log::info!(
            target: "GamingChairLLC.sweep",
            "Sweep on (dwell {:.2}s, key {})",
            settings.dwell,
            if settings.keybind.is_empty() {
                "none"
            } else {
                &settings.keybind
            }
        );

        Ok(())
    }

    fn on_tick(&self, _roblox: &Arc<Roblox>, _now: f64) {
        // The worker owns the teleport loop.
    }

    fn on_values(&self, _roblox: &Arc<Roblox>, values: &Value) {
        *self.settings.lock() = SweepSettings::from_values(values);
    }

    fn on_stop(&self, _roblox: &Arc<Roblox>) {
        self.abort.store(true, Ordering::Release);

        let deadline = Instant::now() + ABORT_TIMEOUT;
        while !self.done.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }

        if self.done.load(Ordering::Acquire) {
            if let Some(handle) = self.worker.lock().take() {
                let _ = handle.join();
            }
        }

        log::info!(target: "GamingChairLLC.sweep", "Sweep off");
    }

    fn take_finished(&self) -> bool {
        self.finished.swap(false, Ordering::AcqRel)
    }
}

fn run(
    roblox: &Arc<Roblox>,
    session: &Arc<FlySession>,
    shared: &SharedSession,
    dwell: f64,
    vk: Option<i32>,
    abort: &AtomicBool,
    done: &AtomicBool,
) {
    let start = session.abs_pos();
    let targets = targets(roblox, session);

    if targets.is_empty() {
        log::info!(target: "GamingChairLLC.sweep", "Nobody else is spawned");
        done.store(true, Ordering::Release);
        return;
    }

    if !session.claim_position("sweep") {
        log::info!(target: "GamingChairLLC.sweep", "Position already claimed; not sweeping");
        done.store(true, Ordering::Release);
        return;
    }

    log::info!(
        target: "GamingChairLLC.sweep",
        "Sweeping {} player(s)",
        targets.len()
    );

    let mut aborted = false;
    for (index, (name, root)) in targets.iter().enumerate() {
        if abort.load(Ordering::Acquire) || !same_session(shared, session) {
            aborted = true;
            break;
        }

        log::info!(
            target: "GamingChairLLC.sweep",
            "  [{}/{}] {}",
            index + 1,
            targets.len(),
            name
        );

        if !pin_to(session, root, dwell, vk, abort, shared) {
            aborted = true;
            break;
        }
    }

    if aborted {
        log::info!(target: "GamingChairLLC.sweep", "  aborted");
    }

    // Always put the player back, even after an abort.
    session.set_abs_pos(start);
    session.release_position("sweep");

    if let Some(start) = start {
        log::info!(
            target: "GamingChairLLC.sweep",
            "Sweep done, back at ({:.1}, {:.1}, {:.1})",
            start.x,
            start.y,
            start.z
        );
    }

    done.store(true, Ordering::Release);
}

/// Hold `abs_pos` on a target for `dwell` seconds. False if interrupted.
fn pin_to(
    session: &Arc<FlySession>,
    root: &Part,
    dwell: f64,
    vk: Option<i32>,
    abort: &AtomicBool,
    shared: &SharedSession,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs_f64(dwell);
    let mut released = vk.map(|vk| !key_down(vk)).unwrap_or(true);

    while Instant::now() < deadline {
        if abort.load(Ordering::Acquire) || !same_session(shared, session) {
            return false;
        }

        if let Some(vk) = vk {
            let down = key_down(vk);
            if released && down {
                return false;
            }
            released = released || !down;
        }

        // Re-read every tick so the pin follows a player who walks off.
        if let Some(position) = root.position() {
            session.set_abs_pos(Some(position));
        }

        std::thread::sleep(PIN_TICK);
    }

    true
}

fn same_session(shared: &SharedSession, session: &Arc<FlySession>) -> bool {
    shared
        .get()
        .map(|current| Arc::ptr_eq(&current, session))
        .unwrap_or(false)
}

/// `(name, root part)` for every other player who has spawned.
fn targets(roblox: &Arc<Roblox>, session: &Arc<FlySession>) -> Vec<(String, Part)> {
    let Ok(datamodel) = roblox.get_datamodel() else {
        return Vec::new();
    };
    let workspace = datamodel.find_first_child_of_name("Workspace");
    let local = roblox
        .get_local_player()
        .map(|player| player.instance().address());

    let mut targets = Vec::new();
    for player in roblox.get_player_list() {
        if local == Some(player.instance().address()) {
            continue;
        }

        let Some(character) = utils::get_character(&datamodel, &player, workspace.as_ref())
        else {
            continue;
        };
        let Some(root) = utils::get_character_root(&character) else {
            continue;
        };

        // Skip anyone still loading in.
        if root.position().is_none() {
            continue;
        }

        // The local character may have respawned since Fly started; keep the
        // session's own root in that case.
        if root.instance().address() == session.root.instance().address() {
            continue;
        }

        targets.push((player.get_name(), root));
    }

    targets
}

pub fn default_values() -> Value {
    json!({
        "keybind": "R",
        "dwell": 0.5,
    })
}
