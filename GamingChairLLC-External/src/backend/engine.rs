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
use super::worker::BackendLoop;

pub struct Engine {
    state: Arc<BackendState>,
    cheats: Arc<Cheats>,
    explorer: Explorer,
    backend: BackendLoop,
    paths: Paths,
    lock: Mutex<()>,
}

impl Engine {
    pub fn new(paths: Paths, events: EventBus) -> Arc<Self> {
        let state = BackendState::new();
        let cheats = Arc::new(Cheats::new());
        let backend = BackendLoop::new(Arc::clone(&state), Arc::clone(&cheats), events);

        Arc::new(Self {
            state,
            cheats,
            explorer: Explorer::new(),
            backend,
            paths,
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

        log::info!(
            target: "GamingChairLLC.engine",
            "Attached (PID: {pid})"
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
        let _guard = self.lock.lock();

        if let Some(cheat) = self.cheats.find_toggle(name) {
            let result = if enabled {
                cheat.enable(&roblox, values)
            } else {
                cheat.disable(&roblox)
            };

            return result_to_json(result);
        }

        if let Some(cheat) = self.cheats.find_ticked(name) {
            if enabled {
                cheats::ticked_enable(cheat.as_ref(), Some(values));
            } else {
                cheats::ticked_disable(cheat.as_ref());
            }
            self.state.bump_epoch();
            return ok();
        }

        error(format!("Cheat '{name}' not found"))
    }

    pub fn set_cheat_values(&self, name: &str, values: Value) -> Value {
        let values = clean_values(Some(values));
        let roblox = self.state.roblox();
        let _guard = self.lock.lock();

        if let Some(cheat) = self.cheats.find_toggle(name) {
            return result_to_json(cheat.update_values(roblox.as_ref(), values));
        }

        if let Some(cheat) = self.cheats.find_ticked(name) {
            cheats::ticked_set_values(cheat.as_ref(), values);
            self.state.bump_epoch();
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
