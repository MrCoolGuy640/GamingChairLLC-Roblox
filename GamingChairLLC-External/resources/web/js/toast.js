// ---------------------------------------------
// Toast: short, non-blocking feedback for background actions.
//
//   Toast.show("Decompiling Foo…", "info", 0);   // sticky
//   Toast.show("Written to C:\\...", "success");  // 4 s
//   Toast.show("Decompile failed: …", "error", 8000);
//   Toast.clear();
//
// Only one toast is visible at a time; showing a new one replaces it. That is
// what lets a "working…" toast be swapped for its result without a stack of
// stale messages piling up.
// ---------------------------------------------
(function () {
  "use strict";

  const DEFAULT_TIMEOUT = 4000;

  let current = null;
  let timer = null;

  function clear() {
    if (timer) {
      clearTimeout(timer);
      timer = null;
    }
    if (current) {
      current.remove();
      current = null;
    }
  }

  function show(message, kind = "info", timeoutMs = DEFAULT_TIMEOUT) {
    clear();

    const toast = document.createElement("div");
    toast.className = "toast toast-" + kind;
    toast.setAttribute("role", "status");
    toast.textContent = message;
    document.body.appendChild(toast);
    current = toast;

    if (timeoutMs > 0) {
      timer = setTimeout(() => {
        timer = null;
        if (current === toast) current = null;
        toast.remove();
      }, timeoutMs);
    }

    return toast;
  }

  window.Toast = { show, clear };
})();
