//! WebView glue: the pywebview-compatible JavaScript shim, IPC parsing and the
//! small `evaluate_script` builders used to send results/events back.

use serde_json::Value;

/// Injected before any page script runs. It exposes the same surface the
/// frontend expects from pywebview (`window.pywebview.api.*` plus the
/// `pywebviewready` event) on top of wry's `window.ipc`.
pub const INIT_SCRIPT: &str = r#"
(function () {
  "use strict";

  var pending = new Map();
  var nextId = 1;

  var METHODS = [
    "get_status",
    "get_loop_stats",
    "get_bootstrap",
    "save_settings",
    "switch_profile",
    "create_profile",
    "delete_profile",
    "open_app_data_folder",
    "attach",
    "detach",
    "get_explorer_tree",
    "get_explorer_children",
    "search_explorer",
    "get_properties",
    "get_cheats",
    "toggle_cheat",
    "set_cheat_values"
  ];

  window.__gc_resolve = function (id, json) {
    var entry = pending.get(id);
    if (!entry) return;
    pending.delete(id);
    var value;
    try {
      value = JSON.parse(json);
    } catch (err) {
      entry.reject(err);
      return;
    }
    entry.resolve(value);
  };

  window.__gc_emit = function (name, json) {
    var data = null;
    try {
      data = JSON.parse(json);
    } catch (err) {
      data = null;
    }
    if (window.host && typeof window.host._emit === "function") {
      window.host._emit(name, data);
    }
  };

  var api = {};
  METHODS.forEach(function (method) {
    api[method] = function () {
      var args = Array.prototype.slice.call(arguments);
      return new Promise(function (resolve, reject) {
        var id = nextId++;
        pending.set(id, { resolve: resolve, reject: reject });
        window.ipc.postMessage(JSON.stringify({ id: id, method: method, args: args }));
      });
    };
  });

  window.pywebview = { api: api };

  function announce() {
    window.dispatchEvent(new Event("pywebviewready"));
  }

  if (document.readyState === "complete") {
    setTimeout(announce, 0);
  } else {
    window.addEventListener("load", function () {
      setTimeout(announce, 0);
    });
  }
})();
"#;

pub struct IpcRequest {
    pub id: u64,
    pub method: String,
    pub args: Vec<Value>,
}

pub fn parse_ipc(body: &str) -> Option<IpcRequest> {
    let value: Value = serde_json::from_str(body).ok()?;

    Some(IpcRequest {
        id: value.get("id")?.as_u64()?,
        method: value.get("method")?.as_str()?.to_string(),
        args: value
            .get("args")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    })
}

/// Resolve a pending JS promise. `result` is serialised twice on purpose: once
/// as the JSON text the shim has to parse, and once as a JS string literal.
pub fn resolve_js(id: u64, result: &Value) -> String {
    let json = serde_json::to_string(result).unwrap_or_else(|_| "null".to_string());
    let literal = serde_json::to_string(&json).unwrap_or_else(|_| "\"null\"".to_string());

    format!("window.__gc_resolve && window.__gc_resolve({id}, {literal});")
}

pub fn emit_js(name: &str, data: &Value) -> String {
    let json = serde_json::to_string(data).unwrap_or_else(|_| "null".to_string());
    let literal = serde_json::to_string(&json).unwrap_or_else(|_| "\"null\"".to_string());
    let name = serde_json::to_string(name).unwrap_or_else(|_| "\"\"".to_string());

    format!("window.__gc_emit && window.__gc_emit({name}, {literal});")
}
