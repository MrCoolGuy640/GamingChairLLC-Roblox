// ---------------------------------------------
// Settings: drill into a panel / back to the list
// ---------------------------------------------
document.querySelectorAll(".settings-row").forEach((row) => {
  row.addEventListener("click", () => {
    const target = row.dataset.panel;
    settingsList.classList.add("is-hidden");
    settingsPanels.forEach((p) => p.classList.toggle("is-active", p.id === `panel-${target}`));
  });
});

document.querySelectorAll(".panel-back").forEach((btn) => {
  btn.addEventListener("click", () => {
    settingsPanels.forEach((p) => p.classList.remove("is-active"));
    settingsList.classList.remove("is-hidden");
  });
});

// ---------------------------------------------
// Generic settings sliders (font size, UI scale...). Only the ones inside the
// Settings view: the WalkSpeed slider (Global view) is wired by walkspeed.js
// because it also talks to Python. Both use wireSliderControl().
// ---------------------------------------------
document.querySelectorAll("#view-settings .slider-control").forEach((control) => {
  wireSliderControl(control);
});

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

document.querySelectorAll(".keybind-btn").forEach((btn) => {
  btn.addEventListener("click", () => {
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

      dbg("Keybind captured:", label);
      btn.textContent = label;
      btn.dataset.key = label;
    }

    window.addEventListener("keydown", handleKey, true);
  });
});
