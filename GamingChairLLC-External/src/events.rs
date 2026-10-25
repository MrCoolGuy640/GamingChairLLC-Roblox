//! Python -> JavaScript push channel.
//!
//! Events are queued onto the native event loop, which performs the actual
//! `evaluate_script` call on the UI thread. `emit` is therefore safe from any
//! thread, including the backend loop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::Value;
use tao::event_loop::EventLoopProxy;

/// Messages delivered to the tao event loop.
pub enum AppEvent {
    /// Evaluate raw JavaScript on the main thread.
    Eval(String),
    /// Push a named event to `window.host`.
    Emit { name: String, data: Value },
}

#[derive(Clone)]
pub struct EventBus {
    proxy: EventLoopProxy<AppEvent>,
    closed: Arc<AtomicBool>,
}

impl EventBus {
    pub fn new(proxy: EventLoopProxy<AppEvent>) -> Self {
        Self {
            proxy,
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn emit(&self, name: &str, data: Value) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }

        let _ = self.proxy.send_event(AppEvent::Emit {
            name: name.to_string(),
            data,
        });
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }
}
