// ---------------------------------------------
// Profiles (Global page)
//
// A profile stores the values of the global cheats. The backend keeps the
// profile files in %LOCALAPPDATA%\GamingChairLLC\profiles and remembers the
// active one in data.json, so a restart restores the same selection.
// ---------------------------------------------
const profileSelect = document.getElementById("profileSelect");
const profileNewBtn = document.getElementById("profileNewBtn");
const profileDeleteBtn = document.getElementById("profileDeleteBtn");

const profileModal = document.getElementById("profileModal");
const profileModalTitle = document.getElementById("profileModalTitle");
const profileModalText = document.getElementById("profileModalText");
const profileModalInput = document.getElementById("profileModalInput");
const profileModalError = document.getElementById("profileModalError");
const profileModalConfirm = document.getElementById("profileModalConfirm");
const profileModalCancel = document.getElementById("profileModalCancel");

let currentProfile = "";
let profileBusy = false;
let modalBusy = false;
let modalAction = null;

// ---------------------------------------------
// Values
// ---------------------------------------------
function applyCheatValues(cheats) {
  const values = (cheats && cheats.walkspeed && cheats.walkspeed.values) || {};
  const speed = values.speed;

  if (window.cheatControls?.walkspeed && typeof speed === "number") {
    window.cheatControls.walkspeed.setValue(speed);
  }
}

// ---------------------------------------------
// Rendering the bar
// ---------------------------------------------
function setProfileBusy(busy) {
  profileBusy = busy;
  if (profileSelect) profileSelect.disabled = busy;
  if (profileNewBtn) profileNewBtn.disabled = busy;
  if (profileDeleteBtn) {
    profileDeleteBtn.disabled = busy || profileSelect.options.length <= 1;
  }
}

function applyProfiles(payload) {
  if (!payload || payload.success === false) {
    dbgWarn("profile request failed:", payload);
    if (payload && payload.error) alert(payload.error);
    return;
  }

  const names = Array.isArray(payload.profiles) ? payload.profiles : [];
  currentProfile = payload.current_profile || names[0] || "";

  profileSelect.replaceChildren();
  names.forEach((name) => {
    const option = document.createElement("option");
    option.value = name;
    option.textContent = name;
    profileSelect.appendChild(option);
  });
  profileSelect.value = currentProfile;

  setProfileBusy(false);
  applyCheatValues(payload.cheats);
}

// ---------------------------------------------
// Switching
// ---------------------------------------------
profileSelect.addEventListener("change", async () => {
  const wanted = profileSelect.value;
  if (!wanted || wanted === currentProfile) return;

  setProfileBusy(true);
  const result = await host.call("switch_profile", wanted);
  setProfileBusy(false);

  if (!result || result.success === false) {
    profileSelect.value = currentProfile;
    alert("Could not switch profile: " + ((result && result.error) || "Unknown error"));
    return;
  }

  applyProfiles(result);
});

// ---------------------------------------------
// Modal
// ---------------------------------------------
function validateProfileName(name) {
  const value = name.trim();

  if (!value) return "Enter a profile name.";
  if (value.length > 32) return "Use 32 characters or fewer.";
  if (/[\\/:*?"<>|]/.test(value)) return 'Names cannot contain \\ / : * ? " < > |.';
  if (value.startsWith(".") || value.endsWith(".")) {
    return "Names cannot start or end with a period.";
  }
  return "";
}

function showModal({ title, text, withInput, initial, confirmLabel, danger, onConfirm }) {
  profileModalTitle.textContent = title;
  profileModalText.textContent = text;
  profileModalInput.hidden = !withInput;
  profileModalInput.value = initial || "";
  profileModalError.textContent = "";
  profileModalError.hidden = true;
  profileModalConfirm.textContent = confirmLabel;
  profileModalConfirm.className = "btn " + (danger ? "btn-danger-solid" : "btn-primary");
  modalAction = onConfirm;
  profileModal.hidden = false;

  setTimeout(() => {
    if (withInput) {
      profileModalInput.focus();
      profileModalInput.select();
    } else {
      profileModalConfirm.focus();
    }
  }, 0);
}

function closeModal() {
  profileModal.hidden = true;
  modalAction = null;
}

async function confirmModal() {
  if (!modalAction || modalBusy) return;

  modalBusy = true;
  try {
    const error = await modalAction();
    if (error) {
      profileModalError.textContent = error;
      profileModalError.hidden = false;
      return;
    }
    closeModal();
  } finally {
    modalBusy = false;
  }
}

profileModalCancel.addEventListener("click", closeModal);
profileModalConfirm.addEventListener("click", confirmModal);

profileModal.addEventListener("mousedown", (e) => {
  if (e.target === profileModal) closeModal();
});

profileModal.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    e.preventDefault();
    closeModal();
  }
});

profileModalInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    e.preventDefault();
    confirmModal();
  }
});

// ---------------------------------------------
// New
// ---------------------------------------------
profileNewBtn.addEventListener("click", () => {
  if (profileBusy) return;

  showModal({
    title: "New profile",
    text: "Create a profile for your global cheats. It starts with the values from the profile you are using now.",
    withInput: true,
    initial: "",
    confirmLabel: "Create",
    danger: false,
    onConfirm: async () => {
      const name = profileModalInput.value.trim();
      const problem = validateProfileName(name);
      if (problem) return problem;

      setProfileBusy(true);
      const result = await host.call("create_profile", name);
      setProfileBusy(false);

      if (!result || result.success === false) {
        return (result && result.error) || "Could not create the profile.";
      }

      applyProfiles(result);
      return "";
    },
  });
});

// ---------------------------------------------
// Delete (the selected profile is always the active one)
// ---------------------------------------------
profileDeleteBtn.addEventListener("click", () => {
  if (profileBusy || !currentProfile) return;

  showModal({
    title: "Delete profile",
    text: `Delete "${currentProfile}"? The cheat values stored in this profile will be removed.`,
    withInput: false,
    confirmLabel: "Delete",
    danger: true,
    onConfirm: async () => {
      setProfileBusy(true);
      const result = await host.call("delete_profile", currentProfile);
      setProfileBusy(false);

      if (!result || result.success === false) {
        return (result && result.error) || "Could not delete the profile.";
      }

      applyProfiles(result);
      return "";
    },
  });
});
