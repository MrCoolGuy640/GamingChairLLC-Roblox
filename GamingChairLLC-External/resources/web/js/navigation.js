// ---------------------------------------------
// Navigation: switch between sidebar views
// ---------------------------------------------
const navItems = document.querySelectorAll(".nav-item");
const views = document.querySelectorAll(".view");
const settingsLanding = document.getElementById("settingsLanding");
const settingsPanels = document.querySelectorAll(".settings-panel");

navItems.forEach((item) => {
  item.addEventListener("click", () => {
    const target = item.dataset.view;
    dbg("Nav item clicked. target view =", target);

    navItems.forEach((n) => n.classList.remove("is-active"));
    item.classList.add("is-active");

    views.forEach((view) => {
      view.classList.toggle("is-active", view.id === `view-${target}`);
    });

    if (target === "settings") {
      settingsLanding.classList.remove("is-hidden");
      settingsPanels.forEach((p) => p.classList.remove("is-active"));
    }

    if (target === "explorer") {
      loadExplorerTree();
    }
    // (The WalkSpeed controls are wired once at startup by walkspeed.js; all
    // views stay in the DOM, so nothing needs re-initialising here.)
  });
});

// ---------------------------------------------
// Sidebar collapse toggle
// ---------------------------------------------
const sidebar = document.getElementById("sidebar");
const sidebarCollapse = document.getElementById("sidebarCollapse");

sidebarCollapse.addEventListener("click", () => {
  sidebar.classList.toggle("is-collapsed");
});
