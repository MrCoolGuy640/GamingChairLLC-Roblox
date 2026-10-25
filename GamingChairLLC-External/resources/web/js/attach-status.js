// ---------------------------------------------
// Attach button + status
//
// NOTE: this file is named attach-status.js (index.html loads exactly that
// name). Your old copy was "attatch-status.js" (extra t), which is why the
// server logged a 404 for it and the Attach button never did anything.
// ---------------------------------------------
const attachBtn = document.getElementById("attachBtn");
const statusDot = document.getElementById("statusDot");
const statusText = document.getElementById("statusText");

let isAttached = false;
let attachBusy = false;   // ignore clicks while an attach/detach call is running

function setAttachedState(attached) {
  dbg("setAttachedState:", attached);
  isAttached = attached;
  statusDot.classList.toggle("is-live", attached);
  statusText.textContent = attached ? "Attached" : "Not attached";
  attachBtn.classList.toggle("is-attached", attached);
  attachBtn.querySelector("span").textContent = attached ? "Detach" : "Attach";
}

attachBtn.addEventListener("click", async () => {
  if (attachBusy) return;
  attachBusy = true;
  attachBtn.disabled = true;

  try {
    if (isAttached) {
      const result = await host.call("detach");
      if (result && result.success) {
        setAttachedState(false);
        clearExplorerTree();
      }
    } else {
      const result = await host.call("attach");
      if (result && result.success) {
        setAttachedState(true);
        loadExplorerTree();
      } else {
        alert("Attach failed: " + ((result && result.error) || "Unknown error"));
      }
    }
  } finally {
    attachBusy = false;
    attachBtn.disabled = false;
  }
});

// Python can change the attached state on its own (e.g. the target went away):
//   events.emit("attach_state", {"attached": False, "reason": "target_lost"})
host.on("attach_state", (data) => {
  const attached = !!(data && data.attached);
  if (attached === isAttached) return;
  setAttachedState(attached);
  if (attached) loadExplorerTree();
  else clearExplorerTree();
});

// ---------------------------------------------
// App info line in the status bar (version / build)
// ---------------------------------------------
const appInfoLine = document.getElementById("appInfoLine");
let appVersion = "0.1.0";
let appBuildType = "Developer";

function setAppInfo({ version, buildType }) {
  if (version) appVersion = version;
  if (buildType) appBuildType = buildType;
  appInfoLine.textContent = `GamingChairLLC Version ${appVersion} ${appBuildType}`;
}

// On load (and after a page reload) ask Python what is actually going on, so
// the UI can never disagree with the backend. Runs after every script has
// loaded because it uses functions from explorer.js.
document.addEventListener("DOMContentLoaded", async () => {
  const status = await host.call("get_status");
  if (!status || status.success === false) return;
  setAppInfo({ version: status.version, buildType: status.build_type });
  if (status.attached && !isAttached) {
    setAttachedState(true);
    loadExplorerTree();
  }
});
