//! The always-on backend loop.
//!
//! A dedicated thread runs every enabled `TickedCheat` and re-applies it at the
//! configured rate. The loop owns no game connection of its own: it borrows the
//! same `Arc<Roblox>` the API threads use, which keeps attach/detach simple and
//! avoids a second process handle.
//!
//! Lifecycle:
//!     attach -> resume()   wakes the thread and waits until it is running
//!     detach -> pause()    stops ticking and waits for the thread to park
//!     target lost          the thread clears the attachment and emits an event

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use serde_json::json;

use crate::cheats::{self, Cheats};
use crate::config;
use crate::events::EventBus;
use crate::sdk::process;
use crate::sdk::roblox::Roblox;

use super::keybinds::Keybinds;
use super::monitor::{self, GameMonitor, MonitorTick};
use super::state::{BackendState, GamePhase, GameStatus, LifeState};

const LIVENESS_INTERVAL: Duration = Duration::from_secs(1);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const PAUSE_TIMEOUT: Duration = Duration::from_secs(3);
/// Key state is polled at ~125 Hz: fast enough to feel instant, cheap enough
/// that the 1000 Hz tick loop does not spend its time on `GetAsyncKeyState`.
const KEYBIND_POLL_INTERVAL: Duration = Duration::from_millis(8);
/// Both session monitors (log tail + death sampling) run at 10 Hz.
const MONITOR_POLL_INTERVAL: Duration = monitor::POLL_INTERVAL;

struct Control {
    go: AtomicBool,
    quit: AtomicBool,
    wake: Mutex<bool>,
    wake_cv: Condvar,
    active: Mutex<bool>,
    active_cv: Condvar,
}

pub struct BackendLoop {
    state: Arc<BackendState>,
    control: Arc<Control>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl BackendLoop {
    pub fn new(state: Arc<BackendState>, cheats: Arc<Cheats>, events: EventBus) -> Self {
        let control = Arc::new(Control {
            go: AtomicBool::new(false),
            quit: AtomicBool::new(false),
            wake: Mutex::new(false),
            wake_cv: Condvar::new(),
            active: Mutex::new(false),
            active_cv: Condvar::new(),
        });

        let thread_control = Arc::clone(&control);
        let thread_state = Arc::clone(&state);
        let thread_cheats = Arc::clone(&cheats);
        let thread_events = events.clone();

        let handle = thread::Builder::new()
            .name("BackendLoop".into())
            .spawn(move || {
                run(
                    thread_control,
                    thread_state,
                    thread_cheats,
                    thread_events,
                );
            })
            .map_err(|err| log::error!(target: "GamingChairLLC.loop", "Could not start backend loop: {err}"))
            .ok();

        Self {
            state,
            control,
            handle: Mutex::new(handle),
        }
    }

    /// Wake the loop and wait until it is ticking the current connection.
    pub fn resume(&self) -> bool {
        let drop_seq = self.state.drop_seq();
        self.state.set_loop_connected(false);
        self.control.go.store(true, Ordering::Release);
        self.wake();

        let deadline = Instant::now() + CONNECT_TIMEOUT;
        while Instant::now() < deadline {
            if !self.state.attached() || self.state.drop_seq() != drop_seq {
                return false;
            }
            if self.state.loop_connected() {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }

        false
    }

    /// Stop ticking and wait for the loop to finish its `on_stop` cleanup.
    pub fn pause(&self) {
        self.control.go.store(false, Ordering::Release);

        let deadline = Instant::now() + PAUSE_TIMEOUT;
        let mut active = self.control.active.lock();

        while *active {
            let now = Instant::now();
            if now >= deadline {
                log::warn!(
                    target: "GamingChairLLC.loop",
                    "Backend loop still busy after {}s",
                    PAUSE_TIMEOUT.as_secs()
                );
                return;
            }

            self.control.active_cv.wait_for(&mut active, deadline - now);
        }
    }

    pub fn shutdown(&self) {
        self.control.quit.store(true, Ordering::Release);
        self.control.go.store(false, Ordering::Release);
        self.wake();

        if let Some(handle) = self.handle.lock().take() {
            let _ = handle.join();
        }
    }

    fn wake(&self) {
        let mut wake = self.control.wake.lock();
        *wake = true;
        self.control.wake_cv.notify_all();
    }
}

impl Drop for BackendLoop {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run(control: Arc<Control>, state: Arc<BackendState>, cheats: Arc<Cheats>, events: EventBus) {
    log::info!(
        target: "GamingChairLLC.loop",
        "Backend loop started ({})",
        if config::LOOP_TARGET_HZ > 0 {
            format!("paced at {} Hz", config::LOOP_TARGET_HZ)
        } else {
            "uncapped".to_string()
        }
    );

    loop {
        {
            let mut wake = control.wake.lock();
            while !*wake && !control.quit.load(Ordering::Acquire) {
                control.wake_cv.wait(&mut wake);
            }
            *wake = false;
        }

        if control.quit.load(Ordering::Acquire) {
            break;
        }
        if !control.go.load(Ordering::Acquire) {
            continue;
        }

        session(&control, &state, &cheats, &events);
    }

    log::info!(target: "GamingChairLLC.loop", "Backend loop stopped");
}

fn session(
    control: &Control,
    state: &BackendState,
    cheats: &Cheats,
    events: &EventBus,
) {
    let Some(roblox) = state.roblox() else {
        return;
    };

    {
        let mut active = control.active.lock();
        *active = true;
    }
    state.set_loop_connected(true);
    state.set_loop_hz(0.0);

    let timer_resolution = config::LOOP_TARGET_HZ > 0;
    if timer_resolution {
        set_timer_resolution(true);
    }

    let started = Instant::now();
    let mut seen_epoch = u64::MAX; // force a full sync on the first iteration
    let mut last_liveness = Instant::now();
    let mut last_keybind_poll = Instant::now();
    let mut last_monitor_poll = Instant::now();
    let mut keybinds = Keybinds::new();
    let mut hz_count: u64 = 0;
    let mut hz_start = Instant::now();

    // Seed the monitors before the first tick so a session that starts on the
    // home screen (or dead) already holds hacks off.
    let mut monitor = GameMonitor::new(&roblox);
    monitor.poll(&roblox);
    state.set_game_status(monitor.status());
    if !monitor.status().is_live_game() {
        // Attaching into a loaded place with no character is a death, not a
        // leave; pick the setting that matches what the monitor sees.
        let disable = if monitor.status().phase == GamePhase::InGame {
            state.disable_hacks_on_death()
        } else {
            state.disable_hacks_on_leave()
        };
        suspend_hacks(state, cheats, &roblox, events, disable);
    }
    log::info!(
        target: "GamingChairLLC.loop",
        "Session monitor: phase {} | place {} | log {}",
        monitor.status().phase.as_str(),
        monitor.status().place_id,
        monitor
            .log_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "not found".to_string())
    );

    let target_hz = config::LOOP_TARGET_HZ;
    let period = if target_hz > 0 {
        Some(Duration::from_secs_f64(1.0 / target_hz as f64))
    } else {
        None
    };
    let mut next_tick = Instant::now();

    while control.go.load(Ordering::Acquire) && !control.quit.load(Ordering::Acquire) {
        let now = started.elapsed().as_secs_f64();

        // While hacks are suspended (home screen / dead) the desired states are
        // preserved but must not be applied: starting a cheat with no character
        // would fail and switch its switch off. `seen_epoch` deliberately stays
        // stale so the next resume forces a full sync.
        //
        // The same applies in the short window before the death monitor sees a
        // vanished character: hold the sync so a keybind pressed right then is
        // not discarded by a failed start.
        let epoch = state.epoch();
        let character_missing = roblox.get_local_character().is_none();
        let start_pending = cheats
            .ticked()
            .iter()
            .any(|cheat| cheat.meta().enabled() && !cheat.meta().running());

        if epoch != seen_epoch
            && !state.hacks_suspended()
            && !(character_missing && start_pending)
        {
            seen_epoch = epoch;
            sync_cheats(cheats, &roblox, events);
        }

        for cheat in cheats.ticked() {
            if cheats::ticked_tick(cheat.as_ref(), &roblox, now) {
                // A one-shot finished or a cheat disabled itself after a panic.
                events.emit(
                    "cheat_state",
                    json!({ "name": cheat.name(), "enabled": false }),
                );
            }
        }

        if last_keybind_poll.elapsed() >= KEYBIND_POLL_INTERVAL {
            last_keybind_poll = Instant::now();
            keybinds.poll(&roblox, cheats, state, events);
        }

        if last_monitor_poll.elapsed() >= MONITOR_POLL_INTERVAL {
            last_monitor_poll = Instant::now();
            let tick = monitor.poll(&roblox);
            if tick.changed() {
                apply_monitor_tick(state, cheats, &roblox, events, &monitor, tick);
            }
        }

        hz_count += 1;

        if let Some(period) = period {
            next_tick += period;
            let current = Instant::now();

            if next_tick > current {
                let remaining = next_tick - current;
                if remaining > Duration::from_millis(2) {
                    thread::sleep(remaining - Duration::from_millis(1));
                }
                while Instant::now() < next_tick {
                    std::hint::spin_loop();
                }
            } else if current.duration_since(next_tick) > period {
                // Fell behind: don't burst to catch up.
                next_tick = current;
            }
        }

        if last_liveness.elapsed() >= LIVENESS_INTERVAL {
            last_liveness = Instant::now();

            if !process::is_pid_alive(roblox.pid()) {
                drop_attachment(state, cheats, events, &format!("process {} exited", roblox.pid()));
                break;
            }

            let elapsed = hz_start.elapsed().as_secs_f64();
            if elapsed > 0.0 {
                state.set_loop_hz(hz_count as f64 / elapsed);
            }
            hz_count = 0;
            hz_start = Instant::now();
        }
    }

    if timer_resolution {
        set_timer_resolution(false);
    }

    // Undo residual effects. Reverse registry order: Fling and Sweep release
    // the Fly session before Fly tears it down. If the target was lost,
    // `drop_attachment` has already cleared the running flags, so this is a
    // no-op.
    for cheat in cheats.ticked().iter().rev() {
        cheats::ticked_halt(cheat.as_ref(), &roblox);
    }

    state.set_loop_hz(0.0);
    state.set_loop_connected(false);
    state.set_hacks_suspended(false);
    state.set_game_status(GameStatus::default());

    {
        let mut active = control.active.lock();
        *active = false;
        control.active_cv.notify_all();
    }
}

/// Turn the monitor transitions into hack lifecycle changes.
fn apply_monitor_tick(
    state: &BackendState,
    cheats: &Cheats,
    roblox: &Arc<Roblox>,
    events: &EventBus,
    monitor: &GameMonitor,
    tick: MonitorTick,
) {
    let status = monitor.status();
    state.set_game_status(status);

    log::info!(
        target: "GamingChairLLC.monitor",
        "Game state: {} | place {} | life {}",
        status.phase.as_str(),
        status.place_id,
        status.life.as_str()
    );

    events.emit(
        "game_state",
        json!({
            "phase": status.phase.as_str(),
            "place_id": status.place_id,
            "life": status.life.as_str(),
        }),
    );

    if tick.game_changed {
        match status.phase {
            // Leaving for the home screen: release every memory lock, and
            // clear the switches too when the setting asks for it.
            GamePhase::Home => {
                suspend_hacks(
                    state,
                    cheats,
                    roblox,
                    events,
                    state.disable_hacks_on_leave(),
                );
            }
            // A join is loading. Nothing exists to apply hacks to yet.
            GamePhase::Joining => {
                suspend_hacks(
                    state,
                    cheats,
                    roblox,
                    events,
                    state.disable_hacks_on_leave(),
                );
            }
            // The place is loaded, but the character may spawn a moment later;
            // the life transition below does the actual resume.
            GamePhase::InGame => {
                if status.life == LifeState::Alive {
                    resume_hacks(state, cheats, roblox);
                }
            }
            GamePhase::Unknown => {}
        }
    }

    if tick.life_changed {
        match status.life {
            LifeState::Dead => {
                suspend_hacks(
                    state,
                    cheats,
                    roblox,
                    events,
                    state.disable_hacks_on_death(),
                );
            }
            LifeState::Alive => {
                if status.phase == GamePhase::InGame {
                    resume_hacks(state, cheats, roblox);
                }
            }
            LifeState::Unknown => {}
        }
    }
}

/// Stop touching the game while keeping (or clearing) the user's switches.
///
/// Every ticked cheat is halted in reverse registry order, which runs its
/// `on_stop` and releases the freezer slots. That is the important part on
/// death: a Fly/Fling slot left behind would keep writing to addresses the
/// respawned character may reuse.
fn suspend_hacks(
    state: &BackendState,
    cheats: &Cheats,
    roblox: &Arc<Roblox>,
    events: &EventBus,
    disable: bool,
) {
    state.set_hacks_suspended(true);

    for cheat in cheats.ticked().iter().rev() {
        let was_enabled = cheat.meta().enabled();
        cheats::ticked_halt_with(cheat.as_ref(), roblox, cheats::HaltMode::Abort);

        if disable && was_enabled {
            cheats::ticked_disable(cheat.as_ref());
            events.emit(
                "cheat_state",
                json!({ "name": cheat.name(), "enabled": false }),
            );
        }
    }

    // Belt and braces: every `on_abort` should have released its slots, but a
    // missed slot would keep writing into memory the client is free to reuse.
    roblox.context().freezer.unfreeze_all();

    if disable {
        for cheat in cheats.toggles() {
            if !cheat.enabled() {
                continue;
            }

            if let Err(err) = cheat.disable(roblox) {
                // The character is usually already gone; the switch still has
                // to be cleared, which `disable` does regardless.
                log::debug!(
                    target: "GamingChairLLC.loop",
                    "Could not restore '{}' while suspending: {err}",
                    cheat.name()
                );
            }
            cheat.reset();
            events.emit(
                "cheat_state",
                json!({ "name": cheat.name(), "enabled": false }),
            );
        }
    }

    state.bump_epoch();
    log::info!(
        target: "GamingChairLLC.loop",
        "Hacks suspended ({})",
        if disable { "switches cleared" } else { "switches kept" }
    );
}

/// Apply the preserved switches once the character is back.
fn resume_hacks(state: &BackendState, cheats: &Cheats, roblox: &Arc<Roblox>) {
    if !state.hacks_suspended() {
        return;
    }

    state.set_hacks_suspended(false);

    // Ticked cheats restart from the epoch sync below. Toggle cheats are not
    // part of that sync, so re-apply the ones that are still switched on.
    // This is also what applies a toggle pressed (via keybind) while dead.
    for cheat in cheats.toggles() {
        if !cheat.enabled() {
            continue;
        }

        if let Err(err) = cheat.enable(roblox, cheat.values()) {
            log::warn!(
                target: "GamingChairLLC.loop",
                "'{}' could not resume: {err}",
                cheat.name()
            );
        }
    }

    state.bump_epoch();
    log::info!(target: "GamingChairLLC.loop", "Hacks resumed");
}

/// Apply the desired states to the running ones and tell the UI what actually
/// happened.
///
/// Starts run in registry order, so Fly is up before Fling/Sweep borrow its
/// session; stops run in reverse, so they have let go before Fly is torn down.
fn sync_cheats(cheats: &Cheats, roblox: &Arc<Roblox>, events: &EventBus) {
    for cheat in cheats.ticked() {
        match cheats::ticked_apply_sync(cheat.as_ref(), roblox) {
            cheats::SyncOutcome::Started => {
                events.emit(
                    "cheat_state",
                    json!({ "name": cheat.name(), "enabled": true }),
                );
            }
            cheats::SyncOutcome::StartFailed(err) => {
                // `ticked_apply_sync` already cleared the desired flag; mirror
                // that to the switch so the UI cannot keep showing "on".
                log::warn!(
                    target: "GamingChairLLC.loop",
                    "'{}' is staying off: {err}",
                    cheat.name()
                );
                events.emit(
                    "cheat_state",
                    json!({ "name": cheat.name(), "enabled": false }),
                );
            }
            cheats::SyncOutcome::Finished => {
                events.emit(
                    "cheat_state",
                    json!({ "name": cheat.name(), "enabled": false }),
                );
            }
            cheats::SyncOutcome::None => {}
        }
    }

    for cheat in cheats.ticked().iter().rev() {
        if cheats::ticked_stop_sync(cheat.as_ref(), roblox) {
            events.emit(
                "cheat_state",
                json!({ "name": cheat.name(), "enabled": false }),
            );
        }
    }
}

fn drop_attachment(
    state: &BackendState,
    cheats: &Cheats,
    events: &EventBus,
    reason: &str,
) {
    log::warn!(target: "GamingChairLLC.loop", "Target lost: {reason}");

    for cheat in cheats.toggles() {
        cheat.reset();
    }
    for cheat in cheats.ticked() {
        let was_enabled = cheat.meta().enabled();
        cheats::ticked_reset(cheat.as_ref());

        if was_enabled {
            events.emit(
                "cheat_state",
                json!({ "name": cheat.name(), "enabled": false }),
            );
        }
    }

    state.dropped(reason);
    events.emit(
        "attach_state",
        json!({ "attached": false, "reason": reason }),
    );
}

/// Windows 1 ms timer while the loop is paced, so `sleep` is accurate.
fn set_timer_resolution(enable: bool) {
    unsafe {
        if enable {
            windows_sys::Win32::Media::timeBeginPeriod(1);
        } else {
            windows_sys::Win32::Media::timeEndPeriod(1);
        }
    }
}
