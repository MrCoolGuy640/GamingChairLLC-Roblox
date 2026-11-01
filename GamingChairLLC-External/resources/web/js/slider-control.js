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
//
// `data-free-input` on the control opts out of the cap for the NUMBER BOX:
// the typed value is used as-is (floored at 0) while the track and fill stay
// clamped to min/max. Used by Fling Speed so 100000 is allowed even though the
// slider stops at 2000.
//
// `data-commit-on-release` keeps a drag visual-only: onChange fires once when
// the thumb is released instead of on every pixel. Used where applying a value
// is expensive (the ESP rebuilds its Highlight objects to change their style).
// ---------------------------------------------
function wireSliderControl(control, { onChange } = {}) {
  const slider = control.querySelector(".slider");
  const input = control.querySelector(".slider-input");
  if (!slider || !input) return null;

  const min = parseFloat(slider.min) || 0;
  const max = parseFloat(slider.max) || 100;
  const step = parseFloat(slider.step) || 1;
  const freeInput = control.hasAttribute("data-free-input");
  const commitOnRelease = control.hasAttribute("data-commit-on-release");

  // What the track and its fill show: always inside the slider's own range.
  function clampToRange(v) {
    if (!Number.isFinite(v)) return min;
    return Math.min(max, Math.max(min, v));
  }

  // Returns a clean in-range number, or null if `v` isn't a number.
  function normalize(v) {
    if (!Number.isFinite(v)) return null;
    v = clampToRange(v);
    v = Math.round((v - min) / step) * step + min;
    v = clampToRange(v);
    return Number(v.toFixed(6));
  }

  // Free input: any positive number, not snapped and not capped at max.
  function normalizeFree(v) {
    if (!Number.isFinite(v)) return null;
    return Number(Math.max(0, v).toFixed(6));
  }

  function paintFill(v) {
    const pct = ((clampToRange(v) - min) / (max - min)) * 100;
    slider.style.background =
      `linear-gradient(to right, var(--white-fill) ${pct}%, var(--bg-4) ${pct}%)`;
  }

  function show(v) {
    const track = clampToRange(v);
    slider.value = track;
    input.value = v;
    paintFill(track);
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
    if (!commitOnRelease && onChange) onChange(v);
  });

  // Released the thumb. Only commit-on-release controls notify here; the
  // others already fired during `input`.
  slider.addEventListener("change", () => {
    if (!commitOnRelease) return;
    const v = normalize(parseFloat(slider.value));
    if (v === null) return;
    value = v;
    input.value = v;
    paintFill(v);
    if (onChange) onChange(v);
  });

  // Typing: preview only (don't rewrite the text while the user is typing)
  input.addEventListener("input", () => {
    const raw = parseFloat(input.value);
    const v = freeInput ? normalizeFree(raw) : normalize(raw);
    if (v === null) return;
    slider.value = clampToRange(v);
    paintFill(v);
  });

  // Commit: Enter / blur / change. Safe to run more than once in a row.
  function commit() {
    const raw = parseFloat(input.value);
    let v = freeInput ? normalizeFree(raw) : normalize(raw);
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
      const n = freeInput ? normalizeFree(v) : normalize(v);
      if (n === null) return;
      value = n;
      show(n);
    },
  };
}
