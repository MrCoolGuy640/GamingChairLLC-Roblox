// ---------------------------------------------
// Slider + number box pair  (.slider-control containing .slider + .slider-input)
//
// One implementation used by every slider in the app (WalkSpeed, settings...).
//
//   const ctl = wireSliderControl(controlEl, { onChange: (value) => {...} });
//   ctl.value        the current committed value
//   ctl.set(v)       change the value from code WITHOUT firing onChange
//
// Behaviour:
//   * Dragging the slider updates the box and calls onChange continuously.
//   * Typing in the box moves the slider preview live, but onChange only fires
//     when the number is COMMITTED: press Enter (leaves the box), click away,
//     or Tab. Empty/invalid text snaps back to the last good value.
//   * Values are clamped to min/max and snapped to the slider's step.
// ---------------------------------------------
function wireSliderControl(control, { onChange } = {}) {
  const slider = control.querySelector(".slider");
  const input = control.querySelector(".slider-input");
  if (!slider || !input) return null;

  const min = parseFloat(slider.min) || 0;
  const max = parseFloat(slider.max) || 100;
  const step = parseFloat(slider.step) || 1;

  // Returns a clean in-range number, or null if `v` isn't a number.
  function normalize(v) {
    if (!Number.isFinite(v)) return null;
    v = Math.min(max, Math.max(min, v));
    v = Math.round((v - min) / step) * step + min;
    v = Math.min(max, Math.max(min, v));
    return Number(v.toFixed(6));
  }

  function paintFill(v) {
    const pct = ((v - min) / (max - min)) * 100;
    slider.style.background =
      `linear-gradient(to right, var(--white-fill) ${pct}%, var(--bg-4) ${pct}%)`;
  }

  function show(v) {
    slider.value = v;
    input.value = v;
    paintFill(v);
  }

  let value = normalize(parseFloat(slider.value));
  if (value === null) value = min;
  show(value);

  // Dragging the slider
  slider.addEventListener("input", () => {
    const v = normalize(parseFloat(slider.value));
    if (v === null) return;
    value = v;
    input.value = v;
    paintFill(v);
    if (onChange) onChange(v);
  });

  // Typing: preview only (don't rewrite the text while the user is typing)
  input.addEventListener("input", () => {
    const v = normalize(parseFloat(input.value));
    if (v === null) return;
    slider.value = v;
    paintFill(v);
  });

  // Commit: Enter / blur / change. Safe to run more than once in a row.
  function commit() {
    let v = normalize(parseFloat(input.value));
    if (v === null) v = value;
    show(v);
    if (v !== value) {
      value = v;
      if (onChange) onChange(v);
    }
  }

  input.addEventListener("change", commit);
  input.addEventListener("blur", commit);
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      input.blur();  // -> commit()
    }
  });

  return {
    get value() { return value; },
    set(v) {
      const n = normalize(v);
      if (n === null) return;
      value = n;
      show(n);
    },
  };
}
