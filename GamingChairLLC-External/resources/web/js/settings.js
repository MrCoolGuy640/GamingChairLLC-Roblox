// ---------------------------------------------
// Settings: panel navigation + persisted preferences
//
// The saved values live in %LOCALAPPDATA%\GamingChairLLC\settings.json (see
// src/backend/store.rs). startup.js calls applySettings() with the values
// loaded by the backend; every change here is written straight back.
// ---------------------------------------------
document.querySelectorAll(".settings-row").forEach((row) => {
  row.addEventListener("click", () => {
    const target = row.dataset.panel;
    settingsLanding.classList.add("is-hidden");
    settingsPanels.forEach((p) => p.classList.toggle("is-active", p.id === `panel-${target}`));
  });
});

document.querySelectorAll(".panel-back").forEach((btn) => {
  btn.addEventListener("click", () => {
    settingsPanels.forEach((p) => p.classList.remove("is-active"));
    settingsLanding.classList.remove("is-hidden");
  });
});

const themeSelect = document.getElementById("themeSelect");
const menuKeybindBtn = document.getElementById("menuKeybindBtn");
const appDataPath = document.getElementById("appDataPath");
const openAppDataBtn = document.getElementById("openAppDataBtn");

// ---------------------------------------------
// Saving (debounced so a burst of changes is one write)
// ---------------------------------------------
let pendingSettings = {};
let settingsTimer = null;

function queueSettingsSave(patch) {
  Object.assign(pendingSettings, patch);
  clearTimeout(settingsTimer);
  settingsTimer = setTimeout(flushSettings, 200);
}

function flushSettings() {
  const patch = pendingSettings;
  pendingSettings = {};
  if (Object.keys(patch).length === 0) return;

  host.call("save_settings", patch).then((result) => {
    if (!result || result.success === false) {
      dbgWarn("save_settings failed:", result);
    }
  });
}

// ---------------------------------------------
// Applying saved values (called by startup.js)
// ---------------------------------------------
function applySettings(settings) {
  if (!settings) return;

  if (themeSelect && settings.theme) {
    themeSelect.value = settings.theme;
    document.documentElement.dataset.theme = settings.theme;
  }

  if (menuKeybindBtn && settings.menu_keybind) {
    menuKeybindBtn.textContent = settings.menu_keybind;
    menuKeybindBtn.dataset.key = settings.menu_keybind;
  }
}

function applyAppDataPath(path) {
  if (!appDataPath) return;
  appDataPath.textContent = path || "Unavailable";
  appDataPath.title = path || "";
}

// ---------------------------------------------
// Controls
// ---------------------------------------------
if (themeSelect) {
  themeSelect.addEventListener("change", () => {
    document.documentElement.dataset.theme = themeSelect.value;
    queueSettingsSave({ theme: themeSelect.value });
  });
}

if (openAppDataBtn) {
  openAppDataBtn.addEventListener("click", async () => {
    openAppDataBtn.disabled = true;
    const result = await host.call("open_app_data_folder");
    openAppDataBtn.disabled = false;

    if (!result || result.success === false) {
      alert(
        "Could not open the app data folder: " +
          ((result && result.error) || "Unknown error")
      );
    }
  });
}

// ---------------------------------------------
// Keybind capture buttons
// ---------------------------------------------
const keyNameMap = {
  " ": "Space",
  "Control": "Ctrl",
  "Escape": "Esc",
  "ArrowUp": "Up",
  "ArrowDown": "Down",
  "ArrowLeft": "Left",
  "ArrowRight": "Right",
};

// ---------------------------------------------
// Keybind capture button (menu keybind only)
//
// Cheat keybinds have their own control in keybinds.js. This block must stay
// scoped to #menuKeybindBtn so the two handlers never run for the same click.
// ---------------------------------------------
if (menuKeybindBtn) {
  menuKeybindBtn.addEventListener("click", () => {
    const btn = menuKeybindBtn;
    if (btn.classList.contains("is-listening")) return;

    const previousLabel = btn.textContent;
    btn.classList.add("is-listening");
    btn.textContent = "Press a key…";

    function handleKey(e) {
      e.preventDefault();
      window.removeEventListener("keydown", handleKey, true);
      btn.classList.remove("is-listening");

      if (e.key === "Escape") {
        btn.textContent = previousLabel;
        return;
      }

      const label = e.key.length === 1
        ? e.key.toUpperCase()
        : (keyNameMap[e.key] || e.key);

      dbg("Menu keybind captured:", label);
      btn.textContent = label;
      btn.dataset.key = label;
      queueSettingsSave({ menu_keybind: label });
    }

    window.addEventListener("keydown", handleKey, true);
  });
}
