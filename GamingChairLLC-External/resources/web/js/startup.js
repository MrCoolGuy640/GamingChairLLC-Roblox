// ---------------------------------------------
// Startup: load saved settings + the active profile
//
// Runs after every other script so applySettings() and applyProfiles() (and
// window.cheatControls, built by walkspeed.js) already exist.
// ---------------------------------------------
(async function bootstrapUi() {
  const result = await host.call("get_bootstrap");

  if (!result || result.success === false) {
    dbgWarn("get_bootstrap failed:", result);
    if (typeof setProfileBusy === "function") setProfileBusy(false);
    return;
  }

  applyAppDataPath(result.app_data_dir || "");
  applySettings(result.settings || {});
  applyProfiles(result);
})();
