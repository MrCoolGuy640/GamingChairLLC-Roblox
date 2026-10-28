// ---------------------------------------------
// Generic right-click menu.
//
//   ContextMenu.show({
//     x, y,                       // client coordinates of the click
//     items: [
//       { label: "Dump As…", onSelect: fn, disabled?: bool },
//       { separator: true },
//     ],
//   });
//   ContextMenu.hide();
//
// One menu exists at a time. It closes on an item click, a pointer press
// outside of it, Escape, scroll, resize, or losing window focus. Nothing in
// here knows about the Explorer: any panel can reuse it by passing its own
// items.
// ---------------------------------------------
(function () {
  "use strict";

  let menu = null;
  let teardown = null;

  function hide() {
    if (!menu) return;

    const closed = menu;
    const restore = teardown;
    menu = null;
    teardown = null;

    closed.remove();
    if (restore) restore();
  }

  // Keep the menu inside the viewport, flipping only when it would overflow.
  function place(node, x, y) {
    const margin = 8;
    const rect = node.getBoundingClientRect();

    let left = x;
    let top = y;
    if (left + rect.width + margin > window.innerWidth) {
      left = Math.max(margin, window.innerWidth - rect.width - margin);
    }
    if (top + rect.height + margin > window.innerHeight) {
      top = Math.max(margin, window.innerHeight - rect.height - margin);
    }

    node.style.left = left + "px";
    node.style.top = top + "px";
  }

  function buildItem(item) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "context-menu-item";
    button.setAttribute("role", "menuitem");
    button.textContent = item.label;
    if (item.disabled) button.disabled = true;

    button.addEventListener("click", (event) => {
      event.preventDefault();
      event.stopPropagation();

      const action = item.onSelect;
      hide();
      if (typeof action === "function") action();
    });

    return button;
  }

  function show(options) {
    hide();

    const items = options && options.items;
    if (!Array.isArray(items) || items.length === 0) return;

    const node = document.createElement("div");
    node.className = "context-menu";
    node.setAttribute("role", "menu");

    items.forEach((item) => {
      if (!item || item.separator) {
        const line = document.createElement("div");
        line.className = "context-menu-separator";
        line.setAttribute("role", "separator");
        node.appendChild(line);
      } else {
        node.appendChild(buildItem(item));
      }
    });

    // Render hidden at the origin first, so measuring is not affected by
    // clamping, then move it into place before it becomes visible.
    node.style.left = "0px";
    node.style.top = "0px";
    node.style.visibility = "hidden";
    document.body.appendChild(node);

    place(node, options.x || 0, options.y || 0);
    node.style.visibility = "";
    menu = node;

    const onPointerDown = (event) => {
      const target = event.target;
      if (!target || !target.closest || !target.closest(".context-menu")) hide();
    };
    const onKeyDown = (event) => {
      if (event.key === "Escape") {
        event.preventDefault();
        hide();
      }
    };
    const onDismiss = () => hide();

    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onKeyDown, true);
    document.addEventListener("scroll", onDismiss, true);
    window.addEventListener("resize", onDismiss);
    window.addEventListener("blur", onDismiss);

    teardown = () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("keydown", onKeyDown, true);
      document.removeEventListener("scroll", onDismiss, true);
      window.removeEventListener("resize", onDismiss);
      window.removeEventListener("blur", onDismiss);
    };

    const first = node.querySelector(".context-menu-item:not(:disabled)");
    if (first) first.focus({ preventScroll: true });
  }

  window.ContextMenu = { show, hide };
})();
