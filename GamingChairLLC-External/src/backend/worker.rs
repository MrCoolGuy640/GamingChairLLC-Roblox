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

use super::state::BackendState;

const LIVENESS_INTERVAL: Duration = Duration::from_secs(1);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const PAUSE_TIMEOUT: Duration = Duration::from_secs(3);

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
    let mut hz_count: u64 = 0;
    let mut hz_start = Instant::now();

    let target_hz = config::LOOP_TARGET_HZ;
    let period = if target_hz > 0 {
        Some(Duration::from_secs_f64(1.0 / target_hz as f64))
    } else {
        None
    };
    let mut next_tick = Instant::now();

    while control.go.load(Ordering::Acquire) && !control.quit.load(Ordering::Acquire) {
        let now = started.elapsed().as_secs_f64();

        let epoch = state.epoch();
        if epoch != seen_epoch {
            seen_epoch = epoch;
            for cheat in cheats.ticked() {
                cheats::ticked_apply_sync(cheat.as_ref(), &roblox);
            }
        }

        for cheat in cheats.ticked() {
            cheats::ticked_tick(cheat.as_ref(), &roblox, now);
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

    // Undo residual effects. If the target was lost, `drop_attachment` has
    // already cleared the running flags, so this is a no-op.
    for cheat in cheats.ticked() {
        cheats::ticked_halt(cheat.as_ref(), &roblox);
    }

    state.set_loop_hz(0.0);
    state.set_loop_connected(false);

    {
        let mut active = control.active.lock();
        *active = false;
        control.active_cv.notify_all();
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
        cheats::ticked_reset(cheat.as_ref());
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
