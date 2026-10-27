// ---------------------------------------------
// Cheat keybinds
//
// Each cheat stores its key in its own profile values (`{ "keybind": "F1" }`),
// so switching profiles switches binds with it. This file only captures the
// label, saves it through set_cheat_values, and mirrors the state the Rust
// backend pushes back (host event "cheat_state") into the toggle switches.
//
// The backend is what actually acts on a bind: a key only toggles a cheat while
// the Roblox window is focused and the in-game chat is not.
// ---------------------------------------------
(function () {
  const buttons = document.querySelectorAll(".cheat-keybind");

  const keyNameMap = {
    " ": "Space",
    "Control": "Ctrl",
    "Shift": "Shift",
    "Alt": "Alt",
    "Escape": "Esc",
    "Backspace": "Backspace",
    "Delete": "Delete",
    "Insert": "Insert",
    "Tab": "Tab",
    "Enter": "Enter",
    "Home": "Home",
    "End": "End",
    "PageUp": "PageUp",
    "PageDown": "PageDown",
    "CapsLock": "CapsLock",
    "ArrowUp": "Up",
    "ArrowDown": "Down",
    "ArrowLeft": "Left",
    "ArrowRight": "Right",
  };

  const PUNCTUATION = "-=[]\\;',./`";

  // Canonical label the Rust side understands, or null for keys it ignores.
  function labelForKey(event) {
    const key = event.key;

    if (/^F([1-9]|1[0-9]|2[0-4])$/.test(key)) return key;
    if (/^[a-zA-Z]$/.test(key)) return key.toUpperCase();
    if (/^[0-9]$/.test(key)) return key;
    if (keyNameMap[key]) return keyNameMap[key];
    if (key.length === 1 && PUNCTUATION.includes(key)) return key;

    return null;
  }

  function display(label) {
    return label || "None";
  }

  function setLabel(button, label) {
    button.dataset.key = label || "";
    if (!button.classList.contains("is-listening")) {
      button.textContent = display(label);
    }
  }

  async function save(button, cheat, previous, label) {
    setLabel(button, label);

    const result = await host.queue(`keybind-${cheat}`, "set_cheat_values", cheat, {
      keybind: label,
    });

    if (!result || result.success === false) {
      dbgWarn("Saving the " + cheat + " keybind failed:", result);
      setLabel(button, previous);
    }
  }

  buttons.forEach((button) => {
    const cheat = button.dataset.cheat;
    if (!cheat) return;

    button.textContent = display(button.dataset.key || "");

    button.addEventListener("click", () => {
      if (button.classList.contains("is-listening")) return;

      const previous = button.dataset.key || "";
      button.classList.add("is-listening");
      button.textContent = "Press a key…";

      function stop() {
        window.removeEventListener("keydown", handleKey, true);
        button.classList.remove("is-listening");
      }

      function handleKey(event) {
        if (event.key === "Escape") {
          event.preventDefault();
          stop();
          setLabel(button, previous);
          return;
        }

        if (event.key === "Backspace") {
          event.preventDefault();
          event.stopPropagation();
          stop();
          save(button, cheat, previous, "");
          return;
        }

        const label = labelForKey(event);
        if (!label) return; // not a bindable key: keep listening

        event.preventDefault();
        event.stopPropagation();
        stop();
        dbg("Cheat keybind captured:", cheat, label);
        save(button, cheat, previous, label);
      }

      window.addEventListener("keydown", handleKey, true);
    });
  });

  // Called by profiles.js after a profile payload arrives (startup included).
  window.applyCheatKeybinds = function applyCheatKeybinds(cheats) {
    if (!cheats) return;

    buttons.forEach((button) => {
      const cheat = button.dataset.cheat;
      const values = cheats[cheat] && cheats[cheat].values;
      setLabel(button, values && typeof values.keybind === "string" ? values.keybind : "");
    });
  };

  // A keybind fired in the backend: mirror the new state into the switch.
  host.on("cheat_state", (data) => {
    if (!data || !data.name) return;

    const control = window.cheatControls && window.cheatControls[data.name];
    if (control && typeof control.setEnabled === "function") {
      control.setEnabled(!!data.enabled);
    }
  });
})();
