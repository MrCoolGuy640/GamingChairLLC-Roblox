//! The JS-facing API. Keep it thin: validate arguments, delegate to `Engine`,
//! return a JSON value. Any failure becomes `{ "success": false, "error": ... }`
//! so the JavaScript side never sees a rejected promise.

use serde_json::{json, Value};

use crate::backend::Engine;

pub fn dispatch(engine: &Engine, method: &str, args: &[Value]) -> Value {
    match method {
        "get_status" => engine.status(),
        "get_loop_stats" => engine.loop_stats(),
        "get_bootstrap" => engine.bootstrap(),
        "save_settings" => engine.save_settings(
            args.first().cloned().unwrap_or_else(|| json!({})),
        ),
        "switch_profile" => engine.switch_profile(&string_arg(args, 0)),
        "create_profile" => engine.create_profile(&string_arg(args, 0)),
        "delete_profile" => engine.delete_profile(&string_arg(args, 0)),
        "open_app_data_folder" => engine.open_app_data_folder(),

        "attach" => engine.attach(),
        "detach" => engine.detach(),

        "get_explorer_tree" => engine.get_explorer_tree(),
        "get_explorer_children" => engine.get_explorer_children(&string_arg(args, 0)),
        "search_explorer" => engine.search_explorer(&string_arg(args, 0)),
        "get_properties" => engine.get_properties(&string_arg(args, 0)),
        "dump_script" => engine.dump_script(&string_arg(args, 0), bool_arg(args, 1, false)),
        "decompile_script" => {
            engine.decompile_script(&string_arg(args, 0), bool_arg(args, 1, false))
        }

        "get_cheats" => engine.get_cheats(),
        "toggle_cheat" => engine.toggle_cheat(
            &string_arg(args, 0),
            args.get(1).and_then(Value::as_bool).unwrap_or(false),
            args.get(2)
                .filter(|value| !value.is_null())
                .cloned(),
        ),
        "set_cheat_values" => engine.set_cheat_values(
            &string_arg(args, 0),
            args.get(1).cloned().unwrap_or_else(|| json!({})),
        ),

        other => json!({
            "success": false,
            "error": format!("Unknown API method: {other}"),
        }),
    }
}

fn string_arg(args: &[Value], index: usize) -> String {
    match args.get(index) {
        Some(Value::String(text)) => text.clone(),
        Some(value) => value.to_string(),
        None => String::new(),
    }
}

fn bool_arg(args: &[Value], index: usize, fallback: bool) -> bool {
    args.get(index).and_then(Value::as_bool).unwrap_or(fallback)
}
