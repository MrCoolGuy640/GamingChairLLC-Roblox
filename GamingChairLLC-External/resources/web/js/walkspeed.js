// ---------------------------------------------
// WalkSpeed toggle + slider/number box
//
// The slider/box ALWAYS shows the value the user picked. It is never reset by
// toggling, never overwritten by a reply from Python, and never asks Python
// for it: Python remembers the game's original walk speed and restores it
// when the toggle goes off (see Engine.toggle_cheat in src/backend/engine.py).
//
// Wired ONCE at startup. All views stay in the DOM, so there is no need to
// re-initialise when navigating (that was causing the "already bound" warnings).
//
// Every call goes through the "walkspeed" channel: strictly one at a time and
// in order, and a burst of slider moves is collapsed to the newest value.
// ---------------------------------------------
(function () {
  const toggle = document.getElementById("walkspeedToggle");
  const control = document.querySelector("#view-global .slider-control");

  if (!toggle || !control) {
    dbgWarn("WalkSpeed elements missing from the page; WalkSpeed UI not wired");
    return;
  }

  let enabled = false;

  const sliderCtl = wireSliderControl(control, {
    onChange(value) {
      // The value is always sent, even while WalkSpeed is off: the backend
      // stores it and writes it into the active profile, and only touches the
      // game when the cheat is enabled.
      host.queueLatest("walkspeed", "set_cheat_values", "walkspeed", {speed: value});
    },
  });

  toggle.addEventListener("change", async () => {
    const wanted = toggle.checked;
    enabled = wanted;

    const result = wanted
      ? await host.queue("walkspeed", "toggle_cheat", "walkspeed", true, {speed: sliderCtl.value})
      : await host.queue("walkspeed", "toggle_cheat", "walkspeed", false);

    if (!result || !result.success) {
      dbgWarn("toggle_cheat failed:", result);
      // Python refused (e.g. not attached): put the switch back, unless the
      // user has already flipped it again in the meantime.
      if (enabled === wanted) {
        enabled = !wanted;
        toggle.checked = !wanted;
      }
    }
  });

  // Handy from the devtools console: __dbg.host.call("get_loop_stats")
  window.__dbg = {
    host,
    get isAttached() { return isAttached; },
    get walkspeedEnabled() { return enabled; },
    get walkspeedValue() { return sliderCtl.value; },
  };

  // Used by the profile UI to mirror a profile's saved speed in the controls
  // without treating it as a user edit, and by keybinds.js when a cheat is
  // toggled from the game.
  window.cheatControls = window.cheatControls || {};
  window.cheatControls.walkspeed = {
    setValue(value) { sliderCtl.set(value); },
    get value() { return sliderCtl.value; },
    setEnabled(next) {
      enabled = !!next;
      toggle.checked = enabled;
    },
    get enabled() { return enabled; },
  };
})();
