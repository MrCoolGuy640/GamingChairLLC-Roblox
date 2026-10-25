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
      // While OFF the value is just remembered by the UI; it gets sent along
      // with the toggle when the user turns WalkSpeed on.
      if (!enabled) return;
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
})();
