// ---------------------------------------------
// Fly / Fling / Sweep / Aim Assist controls
//
// Generic wiring for the controls in index.html:
//
//   .cheat-toggle  data-cheat=...                 -> toggle_cheat
//   .cheat-option  data-cheat=... data-value=...  -> set_cheat_values (checkbox)
//   .cheat-slider  data-cheat=... data-value=...  -> set_cheat_values (slider)
//   .cheat-select  data-cheat=... data-value=...  -> set_cheat_values (dropdown)
//
// Values are always sent, even while a cheat is off: the backend stores them in
// the active profile and only touches the game while the cheat runs.
//
// The Rust side owns the dependency rules (Fling and Sweep force Fly on, Fly
// cannot be turned off while Fling is on). This file only mirrors what the
// backend reports through the "cheat_state" event, which also arrives when a
// prerequisite was switched on for you.
// ---------------------------------------------
(function () {
  const controls = (window.cheatControls = window.cheatControls || {});
  const state = {};

  function ensure(name) {
    if (!state[name]) {
      state[name] = { enabled: false, locked: false, values: {} };
    }
    return state[name];
  }

  // ---------------------------------------------
  // Locking (Fly while Fling is on)
  // ---------------------------------------------
  function setLocked(name, locked) {
    const entry = ensure(name);
    entry.locked = !!locked;

    const elements = [
      document.querySelector(`.cheat-toggle[data-cheat="${name}"]`),
      document.querySelector(`.cheat-keybind[data-cheat="${name}"]`),
    ];

    elements.forEach((element) => {
      if (!element) return;
      element.disabled = entry.locked;

      const row = element.closest(".control-row");
      if (row) row.classList.toggle("is-locked", entry.locked);
    });
  }

  function syncFlyLock() {
    if (!state.fly || !state.fling) return;
    setLocked("fly", state.fling.enabled);
  }

  // ---------------------------------------------
  // Per-cheat control object (keybinds.js mirrors
  // backend state into it, profiles.js loads values)
  // ---------------------------------------------
  function installControl(name) {
    const entry = ensure(name);

    controls[name] = {
      setEnabled(next) {
        entry.enabled = !!next;

        const toggle = document.querySelector(
          `.cheat-toggle[data-cheat="${name}"]`
        );
        if (toggle) toggle.checked = entry.enabled;

        syncFlyLock();
      },
      get enabled() {
        return entry.enabled;
      },
      get locked() {
        return entry.locked;
      },
      setLocked(next) {
        setLocked(name, next);
      },
      setValues(values) {
        if (!values || typeof values !== "object") return;

        Object.assign(entry.values, values);

        document
          .querySelectorAll(`.cheat-slider[data-cheat="${name}"]`)
          .forEach((element) => {
            const field = element.dataset.value;
            const raw = values[field];
            if (typeof raw === "number" && element.__slider) {
              element.__slider.set(raw);
            }
          });

        document
          .querySelectorAll(
            `.cheat-option[data-cheat="${name}"], .cheat-select[data-cheat="${name}"]`
          )
          .forEach((element) => {
            const field = element.dataset.value;
            if (!(field in values)) return;

            if (element.type === "checkbox") {
              element.checked = !!values[field];
            } else {
              element.value = String(values[field]);
            }
          });
      },
    };
  }

  const managed = new Set();
  document
    .querySelectorAll(".cheat-toggle, .cheat-slider, .cheat-option, .cheat-select")
    .forEach((element) => {
      if (element.dataset.cheat) managed.add(element.dataset.cheat);
    });
  managed.forEach(installControl);

  // ---------------------------------------------
  // Toggles
  // ---------------------------------------------
  document.querySelectorAll(".cheat-toggle").forEach((toggle) => {
    const name = toggle.dataset.cheat;
    const control = controls[name];
    if (!control) return;

    toggle.addEventListener("change", async () => {
      if (control.locked) {
        toggle.checked = control.enabled;
        return;
      }

      const wanted = toggle.checked;
      const before = control.enabled;

      // Optimistic: the backend answers either with the same state or with an
      // error (not attached, Fly locked by Fling, ...).
      control.setEnabled(wanted);

      const result = await host.queue(`cheat-${name}`, "toggle_cheat", name, wanted);

      if (!result || result.success === false) {
        dbgWarn(`toggle_cheat ${name} failed:`, result);
        // Only undo if the user has not flipped it again in the meantime.
        if (control.enabled === wanted) control.setEnabled(before);
      }
    });
  });

  // ---------------------------------------------
  // Sliders
  // ---------------------------------------------
  document.querySelectorAll(".cheat-slider").forEach((container) => {
    const name = container.dataset.cheat;
    const field = container.dataset.value;
    if (!name || !field) return;

    const slider = wireSliderControl(container, {
      onChange(value) {
        ensure(name).values[field] = value;
        host.queueLatest(`cheat-values-${name}`, "set_cheat_values", name, {
          [field]: value,
        });
      },
    });

    if (slider) container.__slider = slider;
  });

  // ---------------------------------------------
  // Option switches and dropdowns
  // ---------------------------------------------
  function saveOption(element, value) {
    const name = element.dataset.cheat;
    const field = element.dataset.value;
    if (!name || !field) return;

    ensure(name).values[field] = value;
    host.queueLatest(`cheat-values-${name}`, "set_cheat_values", name, {
      [field]: value,
    });
  }

  document.querySelectorAll(".cheat-option").forEach((element) => {
    element.addEventListener("change", () => saveOption(element, element.checked));
  });

  document.querySelectorAll(".cheat-select").forEach((element) => {
    element.addEventListener("change", () => saveOption(element, element.value));
  });
})();
