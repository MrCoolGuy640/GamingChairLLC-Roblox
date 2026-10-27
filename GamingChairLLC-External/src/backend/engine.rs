//! Engine: the logic behind every JS-callable action.
//!
//! The engine owns the shared state, the cheat registry, the explorer cache and
//! the backend loop. API calls arrive on worker threads and are serialised by
//! `lock` where they mutate state; the backend loop never takes that lock.

use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::cheats::{self, Cheats};
use crate::config::{self, Paths};
use crate::events::EventBus;
use crate::sdk::offsets;
use crate::sdk::roblox::Roblox;

use super::explorer::Explorer;
use super::state::BackendState;
use super::store::Store;
use super::worker::BackendLoop;

pub struct Engine {
    state: Arc<BackendState>,
    cheats: Arc<Cheats>,
    explorer: Explorer,
    backend: BackendLoop,
    store: Store,
    paths: Paths,
    events: EventBus,
    lock: Mutex<()>,
}

impl Engine {
    pub fn new(paths: Paths, events: EventBus) -> Arc<Self> {
        let state = BackendState::new();
        let cheats = Arc::new(Cheats::new());
        let backend = BackendLoop::new(
            Arc::clone(&state),
            Arc::clone(&cheats),
            events.clone(),
        );

        // Restore the profile that was active last time (or create it).
        let store = Store::new(paths.app_data.clone());
        let current = store.ensure_current(&cheats.values_snapshot());
        cheats.apply_values(&store.profile_cheats(&current));

        log::info!(
            target: "GamingChairLLC.engine",
            "Using profile '{current}' ({})",
            store.root().display()
        );

        Arc::new(Self {
            state,
            cheats,
            explorer: Explorer::new(),
            backend,
            store,
            paths,
            events,
            lock: Mutex::new(()),
        })
    }

    // -- lifecycle -------------------------------------------------------

    pub fn shutdown(&self) {
        let _guard = self.lock.lock();
        log::info!(target: "GamingChairLLC.engine", "Engine shutting down");

        self.disable_all();
        self.backend.shutdown();
        self.state.set_attached(false);
        self.state.set_roblox(None);
    }

    // -- status ----------------------------------------------------------

    pub fn status(&self) -> Value {
        json!({
            "attached": self.state.attached(),
            "version": config::APP_VERSION,
            "build_type": config::BUILD_TYPE,
        })
    }

    pub fn loop_stats(&self) -> Value {
        json!({
            "attached": self.state.attached(),
            "loop_hz": self.state.loop_hz(),
        })
    }

    // -- persistence -----------------------------------------------------

    /// Everything the UI needs at startup: preferences, profiles and the cheat
    /// values of the profile that was active last time.
    pub fn bootstrap(&self) -> Value {
        let _guard = self.lock.lock();
        let current = self.store.ensure_current(&self.cheats.values_snapshot());

        json!({
            "success": true,
            "app_data_dir": self.store.root().display().to_string(),
            "settings": self.store.settings(),
            "profiles": self.store.profiles(),
            "current_profile": current,
            "cheats": self.cheats.info(),
        })
    }

    pub fn save_settings(&self, patch: Value) -> Value {
        match self.store.save_settings(&patch) {
            Ok(settings) => json!({ "success": true, "settings": settings }),
            Err(err) => error(err),
        }
    }

    pub fn switch_profile(&self, name: &str) -> Value {
        let _guard = self.lock.lock();

        let Some(profile) = self.store.find_profile(name) else {
            return error(format!("Profile '{name}' not found"));
        };

        if let Err(err) = self.store.set_current(&profile) {
            return error(err);
        }

        log::info!(
            target: "GamingChairLLC.engine",
            "Switched to profile '{profile}'"
        );

        self.apply_profile(&profile);
        self.profiles_payload()
    }

    pub fn create_profile(&self, name: &str) -> Value {
        let _guard = self.lock.lock();

        // A new profile starts from what is currently configured.
        let values = self.cheats.values_snapshot();

        match self.store.create_profile(name, &values) {
            Ok(profile) => {
                log::info!(
                    target: "GamingChairLLC.engine",
                    "Created profile '{profile}'"
                );
                self.apply_profile(&profile);
                self.profiles_payload()
            }
            Err(err) => error(err),
        }
    }

    pub fn delete_profile(&self, name: &str) -> Value {
        let _guard = self.lock.lock();

        match self.store.delete_profile(name) {
            Ok(current) => {
                log::info!(
                    target: "GamingChairLLC.engine",
                    "Deleted profile '{name}' (now using '{current}')"
                );
                self.apply_profile(&current);
                self.profiles_payload()
            }
            Err(err) => error(err),
        }
    }

    pub fn open_app_data_folder(&self) -> Value {
        let path = self.store.root();

        if let Err(err) = std::fs::create_dir_all(path) {
            return error(format!("Could not create {}: {err}", path.display()));
        }

        match std::process::Command::new("explorer.exe").arg(path).spawn() {
            Ok(_) => {
                log::info!(
                    target: "GamingChairLLC.engine",
                    "Opened app data folder: {}",
                    path.display()
                );
                ok()
            }
            Err(err) => error(format!("Could not open {}: {err}", path.display())),
        }
    }

    fn profiles_payload(&self) -> Value {
        json!({
            "success": true,
            "profiles": self.store.profiles(),
            "current_profile": self.store.current_profile(),
            "cheats": self.cheats.info(),
        })
    }

    /// Load a profile's values into the cheat registry. Enabled toggles are
    /// re-applied so switching profiles updates the game immediately; ticked
    /// cheats pick the values up on the next loop pass.
    fn apply_profile(&self, profile: &str) {
        let values = self.store.profile_cheats(profile);
        let roblox = self.state.roblox();

        for cheat in self.cheats.toggles() {
            let Some(value) = values.get(cheat.name()) else {
                continue;
            };
            cheat.store_values(value);

            if cheat.enabled() {
                if let Some(roblox) = roblox.as_ref() {
                    if let Err(err) = cheat.enable(roblox, cheat.values()) {
                        log::warn!(
                            target: "GamingChairLLC.engine",
                            "Failed to re-apply '{}' after a profile change: {err}",
                            cheat.name()
                        );
                    }
                }
            }
        }

        for cheat in self.cheats.ticked() {
            if let Some(value) = values.get(cheat.name()) {
                cheats::ticked_set_values(cheat.as_ref(), value.clone());
            }
        }

        self.state.bump_epoch();
    }

    /// Write the current cheat values into the active profile.
    fn persist_cheat_values(&self) {
        let name = self.store.current_profile();
        let values = self.cheats.values_snapshot();

        if let Err(err) = self.store.write_profile(&name, &values) {
            log::warn!(
                target: "GamingChairLLC.engine",
                "Could not save profile '{name}': {err}"
            );
        }
    }

    // -- attach / detach -------------------------------------------------

    pub fn attach(&self) -> Value {
        let _guard = self.lock.lock();

        if self.state.attached() {
            return ok();
        }

        let offsets = match offsets::load(&self.paths) {
            Ok(offsets) => Arc::new(offsets),
            Err(err) => {
                log::error!(target: "GamingChairLLC.engine", "Attach failed: {err:#}");
                return error(format!("{err:#}"));
            }
        };

        let roblox = match Roblox::connect(offsets) {
            Ok(roblox) => roblox,
            Err(err) => {
                log::error!(target: "GamingChairLLC.engine", "Attach failed: {err:#}");
                return error(format!("{err:#}"));
            }
        };

        let pid = roblox.pid();
        self.state.set_roblox(Some(Arc::clone(&roblox)));
        self.state.set_attached(true);

        if !self.backend.resume() {
            let reason = self
                .state
                .drop_reason()
                .trim()
                .to_string();
            let reason = if reason.is_empty() {
                "the backend loop did not connect in time".to_string()
            } else {
                reason
            };

            self.backend.pause();
            self.state.set_attached(false);
            self.state.set_roblox(None);

            log::error!(target: "GamingChairLLC.engine", "Attach failed: {reason}");
            return error(reason);
        }

        let server = roblox.server_ip();
        let server = if server.is_empty() {
            "unknown".to_string()
        } else {
            server
        };

        log::info!(
            target: "GamingChairLLC.engine",
            "Attached (PID: {pid}) | user {} | place {} | game_loaded {} | server {server}",
            roblox.user_id(),
            roblox.place_id(),
            roblox.game_loaded()
        );
        ok()
    }

    pub fn detach(&self) -> Value {
        let _guard = self.lock.lock();

        // Disable first so toggle cheats can still restore values through the
        // live handle, then park the loop (which stops ticked cheats).
        self.disable_all();
        self.backend.pause();

        self.state.set_attached(false);
        self.state.set_roblox(None);
        self.explorer.clear();

        log::info!(target: "GamingChairLLC.engine", "Detached");
        ok()
    }

    fn disable_all(&self) {
        let roblox = self.state.roblox();

        if let Some(roblox) = &roblox {
            for cheat in self.cheats.toggles() {
                if !cheat.enabled() {
                    continue;
                }

                if let Err(err) = cheat.disable(roblox) {
                    log::warn!(
                        target: "GamingChairLLC.engine",
                        "Failed to disable cheat '{}': {err}",
                        cheat.name()
                    );
                }
            }
        }

        for cheat in self.cheats.ticked() {
            if cheat.meta().enabled() {
                cheats::ticked_disable(cheat.as_ref());
            }
        }

        self.state.bump_epoch();
    }

    // -- cheats ----------------------------------------------------------

    pub fn get_cheats(&self) -> Value {
        self.cheats.info()
    }

    pub fn toggle_cheat(&self, name: &str, enabled: bool, values: Option<Value>) -> Value {
        let Some(roblox) = self.state.roblox() else {
            return error("Not attached");
        };

        let values = clean_values(values);
        let changed_values = values
            .as_object()
            .map(|object| !object.is_empty())
            .unwrap_or(false);
        let _guard = self.lock.lock();

        if let Some(cheat) = self.cheats.find_toggle(name) {
            let result = if enabled {
                cheat.enable(&roblox, values)
            } else {
                cheat.disable(&roblox)
            };

            if changed_values {
                self.persist_cheat_values();
            }

            return result_to_json(result);
        }

        let plan = match self.cheats.plan_toggle(name, enabled) {
            Ok(plan) => plan,
            Err(err) => return error(err),
        };

        for change in plan {
            let Some(cheat) = self.cheats.find_ticked(change.name) else {
                continue;
            };

            if change.enabled {
                // Prerequisites inherit Fly's stored values; only the cheat the
                // user clicked receives the values sent with this request.
                let patch = (change.name == name).then(|| values.clone());
                cheats::ticked_enable(cheat.as_ref(), patch);
            } else {
                cheats::ticked_disable(cheat.as_ref());
            }

            // Forced prerequisite changes happen here rather than in the
            // worker, so mirror them immediately instead of waiting a tick.
            self.events.emit(
                "cheat_state",
                json!({ "name": change.name, "enabled": change.enabled }),
            );
        }

        self.state.bump_epoch();

        if changed_values {
            self.persist_cheat_values();
        }

        ok()
    }

    pub fn set_cheat_values(&self, name: &str, values: Value) -> Value {
        let values = clean_values(Some(values));
        let roblox = self.state.roblox();
        let _guard = self.lock.lock();

        if let Some(cheat) = self.cheats.find_toggle(name) {
            let result = cheat.update_values(roblox.as_ref(), values);
            self.persist_cheat_values();
            return result_to_json(result);
        }

        if let Some(cheat) = self.cheats.find_ticked(name) {
            cheats::ticked_set_values(cheat.as_ref(), values);
            self.state.bump_epoch();
            self.persist_cheat_values();
            return ok();
        }

        error(format!("Cheat '{name}' not found"))
    }

    // -- explorer --------------------------------------------------------

    pub fn get_explorer_tree(&self) -> Value {
        let Some(roblox) = self.attached_roblox() else {
            return json!({ "nodes": [], "lookahead": {} });
        };

        self.explorer.tree(&roblox)
    }

    pub fn get_explorer_children(&self, address: &str) -> Value {
        let Some(roblox) = self.attached_roblox() else {
            return json!({ "address": address, "nodes": [], "lookahead": {} });
        };

        self.explorer.children(&roblox, address)
    }

    pub fn search_explorer(&self, query: &str) -> Value {
        let Some(roblox) = self.attached_roblox() else {
            return json!({ "query": query, "results": [] });
        };

        self.explorer.search(&roblox, query)
    }

    pub fn get_properties(&self, address: &str) -> Value {
        let Some(roblox) = self.attached_roblox() else {
            return json!({ "address": address, "properties": {} });
        };

        self.explorer.properties(&roblox, address)
    }

    fn attached_roblox(&self) -> Option<Arc<Roblox>> {
        if !self.state.attached() {
            return None;
        }
        self.state.roblox()
    }
}

fn clean_values(values: Option<Value>) -> Value {
    match values {
        Some(Value::Null) | None => Value::Object(Default::default()),
        Some(value) => value,
    }
}

fn ok() -> Value {
    json!({ "success": true })
}

fn error(message: impl Into<String>) -> Value {
    json!({ "success": false, "error": message.into() })
}

fn result_to_json(result: Result<(), String>) -> Value {
    match result {
        Ok(()) => ok(),
        Err(err) => error(err),
    }
}
