//! Persistent user data.
//!
//! Everything lives in `%LOCALAPPDATA%\GamingChairLLC`:
//!
//! ```text
//! settings.json          UI preferences (theme, keybind)
//! data.json              current profile
//! profiles\<name>.json   cheat values per profile
//! ```
//!
//! Files are written atomically (write to `.tmp`, then rename) so a crash in
//! the middle of a save never leaves a truncated JSON file behind.

use std::fs;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use serde_json::{json, Map, Value};

const DEFAULT_PROFILE: &str = "Default";
const MAX_PROFILE_NAME: usize = 32;

const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

pub struct Store {
    root: PathBuf,
    profiles_dir: PathBuf,
    settings_path: PathBuf,
    state_path: PathBuf,
    settings: Mutex<Value>,
    /// Parsed `data.json`, preserved so unknown keys survive a rewrite.
    state: Mutex<Value>,
    current: Mutex<String>,
}

impl Store {
    pub fn new(root: PathBuf) -> Self {
        let profiles_dir = root.join("profiles");
        let settings_path = root.join("settings.json");
        let state_path = root.join("data.json");

        let _ = fs::create_dir_all(&profiles_dir);

        let stored_settings = read_json(&settings_path);
        let settings = sanitize_settings(merge_objects(
            default_settings(),
            stored_settings.clone().unwrap_or(Value::Null),
        ));

        // Write the defaults on the very first launch (and clean up settings
        // written by older builds) so the file on disk always matches what the
        // app actually uses.
        if stored_settings.as_ref() != Some(&settings) {
            if let Err(err) = write_json(&settings_path, &settings) {
                log::warn!(target: "GamingChairLLC.store", "Could not create settings.json: {err}");
            }
        }

        let mut state = read_json(&state_path).unwrap_or_else(|| json!({}));
        if !state.is_object() {
            state = json!({});
        }

        let current = state
            .get("current_profile")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| is_valid_name(name))
            .unwrap_or(DEFAULT_PROFILE)
            .to_string();

        Self {
            root,
            profiles_dir,
            settings_path,
            state_path,
            settings: Mutex::new(settings),
            state: Mutex::new(state),
            current: Mutex::new(current),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    // -- settings --------------------------------------------------------

    pub fn settings(&self) -> Value {
        self.settings.lock().clone()
    }

    /// Merge `patch` into the stored settings, validate it and write it back.
    pub fn save_settings(&self, patch: &Value) -> Result<Value, String> {
        let mut guard = self.settings.lock();
        let merged = sanitize_settings(merge_objects(guard.clone(), patch.clone()));
        write_json(&self.settings_path, &merged)?;
        *guard = merged.clone();
        Ok(merged)
    }

    // -- profiles --------------------------------------------------------

    pub fn profiles(&self) -> Vec<String> {
        let mut names = Vec::new();

        if let Ok(entries) = fs::read_dir(&self.profiles_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let is_json = path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .map(|extension| extension.eq_ignore_ascii_case("json"))
                    .unwrap_or(false);
                if !is_json {
                    continue;
                }

                if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                    if is_valid_name(stem) {
                        names.push(stem.to_string());
                    }
                }
            }
        }

        names.sort_by_key(|name| name.to_ascii_lowercase());
        names
    }

    pub fn current_profile(&self) -> String {
        self.current.lock().clone()
    }

    /// Case-insensitive lookup. Returns the name as it exists on disk.
    pub fn find_profile(&self, name: &str) -> Option<String> {
        self.profiles()
            .into_iter()
            .find(|profile| profile.eq_ignore_ascii_case(name.trim()))
    }

    pub fn profile_exists(&self, name: &str) -> bool {
        is_valid_name(name) && self.profile_path(name).is_file()
    }

    /// Cheat values stored in a profile (`{}` when the file is missing or bad).
    pub fn profile_cheats(&self, name: &str) -> Value {
        if !is_valid_name(name) {
            return Value::Object(Map::new());
        }

        let Some(value) = read_json(&self.profile_path(name)) else {
            return Value::Object(Map::new());
        };

        match value.get("cheats") {
            Some(cheats) if cheats.is_object() => cheats.clone(),
            _ => Value::Object(Map::new()),
        }
    }

    pub fn write_profile(&self, name: &str, cheats: &Value) -> Result<(), String> {
        let name = normalize_profile_name(name)?;

        let payload = json!({
            "version": 1,
            "name": name,
            "cheats": cheats,
        });
        write_json(&self.profile_path(&name), &payload)
    }

    /// Make sure the profile named in `data.json` exists. Creates it (or falls
    /// back to `Default`) when it was deleted or the state file was edited.
    pub fn ensure_current(&self, default_cheats: &Value) -> String {
        let current = self.current_profile();
        if self.profile_exists(&current) {
            return current;
        }

        if !current.eq_ignore_ascii_case(DEFAULT_PROFILE) && self.profile_exists(DEFAULT_PROFILE) {
            if let Err(err) = self.set_current(DEFAULT_PROFILE) {
                log::warn!(target: "GamingChairLLC.store", "Could not select Default profile: {err}");
            }
            return DEFAULT_PROFILE.to_string();
        }

        let name = if is_valid_name(&current) {
            current
        } else {
            DEFAULT_PROFILE.to_string()
        };

        if let Err(err) = self.write_profile(&name, default_cheats) {
            log::warn!(target: "GamingChairLLC.store", "Could not create profile '{name}': {err}");
        }
        if let Err(err) = self.set_current(&name) {
            log::warn!(target: "GamingChairLLC.store", "Could not select profile '{name}': {err}");
        }
        name
    }

    pub fn set_current(&self, name: &str) -> Result<(), String> {
        let Some(canonical) = self.find_profile(name) else {
            return Err("Profile not found".to_string());
        };

        *self.current.lock() = canonical.clone();

        let mut state = self.state.lock();
        if let Some(object) = state.as_object_mut() {
            object.insert("current_profile".into(), Value::String(canonical));
        }
        write_json(&self.state_path, &state)
    }

    /// Create a profile from the given values and make it current.
    pub fn create_profile(&self, name: &str, cheats: &Value) -> Result<String, String> {
        let name = normalize_profile_name(name)?;

        if self
            .profiles()
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&name))
        {
            return Err(format!("A profile named \"{name}\" already exists"));
        }

        self.write_profile(&name, cheats)?;
        self.set_current(&name)?;
        Ok(name)
    }

    /// Delete a profile and return the profile that is current afterwards.
    pub fn delete_profile(&self, name: &str) -> Result<String, String> {
        let Some(canonical) = self.find_profile(name) else {
            return Err("Profile not found".to_string());
        };

        let profiles = self.profiles();
        if profiles.len() <= 1 {
            return Err("You need to keep at least one profile".to_string());
        }

        fs::remove_file(self.profile_path(&canonical))
            .map_err(|err| format!("Could not delete \"{canonical}\": {err}"))?;

        if canonical.eq_ignore_ascii_case(&self.current_profile()) {
            let next = self
                .profiles()
                .into_iter()
                .next()
                .unwrap_or_else(|| DEFAULT_PROFILE.to_string());
            self.set_current(&next)?;
            return Ok(next);
        }

        Ok(self.current_profile())
    }

    fn profile_path(&self, name: &str) -> PathBuf {
        self.profiles_dir.join(format!("{name}.json"))
    }
}

// ---------------------------------------------------------------------------
// Settings validation
// ---------------------------------------------------------------------------

fn default_settings() -> Value {
    json!({
        "theme": "dark",
        "menu_keybind": "Insert",
    })
}

fn sanitize_settings(value: Value) -> Value {
    let mut object = match value {
        Value::Object(object) => object,
        _ => Map::new(),
    };

    let theme = object
        .get("theme")
        .and_then(Value::as_str)
        .unwrap_or("dark")
        .to_ascii_lowercase();
    let theme = match theme.as_str() {
        "light" | "system" => theme,
        _ => "dark".to_string(),
    };
    object.insert("theme".into(), Value::String(theme));

    let keybind = object
        .get("menu_keybind")
        .and_then(Value::as_str)
        .unwrap_or("Insert")
        .trim()
        .chars()
        .take(32)
        .collect::<String>();
    let keybind = if keybind.is_empty() {
        "Insert".to_string()
    } else {
        keybind
    };
    object.insert("menu_keybind".into(), Value::String(keybind));

    Value::Object(object)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn merge_objects(mut base: Value, patch: Value) -> Value {
    if let (Value::Object(base), Value::Object(patch)) = (&mut base, patch) {
        for (key, value) in patch {
            base.insert(key, value);
        }
    }
    base
}

fn normalize_profile_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();

    if name.is_empty() {
        return Err("Enter a profile name".to_string());
    }
    if name.chars().count() > MAX_PROFILE_NAME {
        return Err(format!(
            "Profile names can be at most {MAX_PROFILE_NAME} characters"
        ));
    }
    if name.chars().any(char::is_control) {
        return Err("Profile names cannot contain control characters".to_string());
    }
    if name.chars().any(|ch| "\\/:*?\"<>|".contains(ch)) {
        return Err("Profile names cannot contain \\ / : * ? \" < > |".to_string());
    }
    if name.starts_with('.') || name.ends_with('.') {
        return Err("Profile names cannot start or end with a period".to_string());
    }

    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    if RESERVED_NAMES.contains(&stem.as_str()) {
        return Err(format!("\"{name}\" is a reserved Windows name"));
    }

    Ok(name.to_string())
}

fn is_valid_name(name: &str) -> bool {
    normalize_profile_name(name).is_ok()
}

fn read_json(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;

    match serde_json::from_str(&text) {
        Ok(value) => Some(value),
        Err(err) => {
            log::warn!(
                target: "GamingChairLLC.store",
                "Ignoring invalid JSON in {}: {err}",
                path.display()
            );
            None
        }
    }
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("Could not create {}: {err}", parent.display()))?;
    }

    let text = serde_json::to_string_pretty(value)
        .map_err(|err| format!("Could not serialise JSON: {err}"))?;

    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "data.json".to_string());
    let temp = path.with_file_name(format!("{file_name}.tmp"));

    fs::write(&temp, text).map_err(|err| format!("Could not write {}: {err}", temp.display()))?;

    if let Err(err) = fs::rename(&temp, path) {
        // Very defensive: on Windows a rename over an existing file normally
        // works, but if it does not, replace it explicitly.
        if path.is_file() {
            let _ = fs::remove_file(path);
            fs::rename(&temp, path)
                .map_err(|err| format!("Could not save {}: {err}", path.display()))?;
        } else {
            let _ = fs::remove_file(&temp);
            return Err(format!("Could not save {}: {err}", path.display()));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{normalize_profile_name, Store, DEFAULT_PROFILE};
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_store(tag: &str) -> (Store, PathBuf) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "gamingchairllc-test-{tag}-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        (Store::new(root.clone()), root)
    }

    #[test]
    fn profile_names_are_sanitised() {
        assert_eq!(normalize_profile_name("  Rage  ").unwrap(), "Rage");
        assert!(normalize_profile_name("").is_err());
        assert!(normalize_profile_name("a/b").is_err());
        assert!(normalize_profile_name("CON").is_err());
        assert!(normalize_profile_name(".hidden").is_err());
        assert!(normalize_profile_name("trailing.").is_err());
        assert!(normalize_profile_name(&"x".repeat(33)).is_err());
    }

    #[test]
    fn settings_are_clamped_and_persisted() {
        let (store, root) = temp_store("settings");

        // Defaults are written on first launch, before anything is changed.
        assert!(root.join("settings.json").is_file());

        let settings = store
            .save_settings(&json!({ "theme": "LIGHT", "menu_keybind": "  F1 " }))
            .unwrap();

        assert_eq!(settings["theme"], "light");
        assert_eq!(settings["menu_keybind"], "F1");

        // A fresh Store sees the same values.
        let reloaded = Store::new(root.clone());
        assert_eq!(reloaded.settings()["theme"], "light");
        assert_eq!(reloaded.settings()["menu_keybind"], "F1");

        // Unknown themes fall back to dark.
        let settings = store.save_settings(&json!({ "theme": "neon" })).unwrap();
        assert_eq!(settings["theme"], "dark");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn profiles_can_be_created_switched_and_deleted() {
        let (store, root) = temp_store("profiles");
        let defaults = json!({ "walkspeed": { "speed": 16 }, "noclip": {} });

        let current = store.ensure_current(&defaults);
        assert_eq!(current, DEFAULT_PROFILE);
        assert!(store.profile_exists(DEFAULT_PROFILE));

        let created = store
            .create_profile("Rage", &json!({ "walkspeed": { "speed": 100 } }))
            .unwrap();
        assert_eq!(created, "Rage");
        assert_eq!(store.current_profile(), "Rage");
        assert_eq!(store.profile_cheats("Rage")["walkspeed"]["speed"], 100.0);

        // Duplicate names are rejected, case-insensitively.
        assert!(store.create_profile("rage", &defaults).is_err());

        // Deleting the current profile falls back to the remaining one.
        let next = store.delete_profile("Rage").unwrap();
        assert_eq!(next, DEFAULT_PROFILE);
        assert_eq!(store.current_profile(), DEFAULT_PROFILE);
        assert!(!store.profile_exists("Rage"));

        // The last profile cannot be deleted.
        assert!(store.delete_profile(DEFAULT_PROFILE).is_err());

        // State survives a restart.
        let reloaded = Store::new(root.clone());
        assert_eq!(reloaded.current_profile(), DEFAULT_PROFILE);

        let _ = fs::remove_dir_all(root);
    }
}
