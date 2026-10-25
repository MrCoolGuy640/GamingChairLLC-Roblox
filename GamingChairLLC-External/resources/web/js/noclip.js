// ---------------------------------------------
// NoClip toggle
//
// Toggles noclip on/off. The backend re-applies collision settings
// every ~7.5 seconds because Roblox reverts them.
// ---------------------------------------------
(function () {
  const toggle = document.getElementById("noclipToggle");

  if (!toggle) {
    dbgWarn("NoClip toggle element missing from the page; NoClip UI not wired");
    return;
  }

  let enabled = false;

  toggle.addEventListener("change", async () => {
    const wanted = toggle.checked;
    enabled = wanted;

    const result = await host.queue("noclip", "toggle_cheat", "noclip", wanted);

    if (!result || !result.success) {
      dbgWarn("toggle_cheat noclip failed:", result);
      // Python refused (e.g. not attached): put the switch back, unless the
      // user has already flipped it again in the meantime.
      if (enabled === wanted) {
        enabled = !wanted;
        toggle.checked = !wanted;
      }
    }
  });

  window.__dbg = window.__dbg || {};
  window.__dbg.noclipEnabled = function () { return enabled; };
})();