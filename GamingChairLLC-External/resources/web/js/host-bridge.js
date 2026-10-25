// ---------------------------------------------
// Host bridge (pywebview)  ->  window.host
//
//   host.call(name, ...args)          -> Promise<result>
//       Calls Python's Api.<name>(...). Waits for the Python side to be
//       ready first, so it is safe to call at any time. It NEVER rejects:
//       failures resolve to { success:false, error:"..." } (same shape the
//       Python side uses), so callers only have to check result.success.
//
//   host.queue(channel, name, ...args)        -> Promise<result>
//   host.queueLatest(channel, name, ...args)  -> Promise<result>
//       Same, but calls on one `channel` run strictly ONE AT A TIME, in
//       order. Why: pywebview runs every call on its own Python thread, so
//       two calls made back to back can finish (or even start) out of order.
//       For anything where "the last value wins" (sliders!) use these.
//       queueLatest additionally collapses a burst of same-name calls that
//       are still waiting into one carrying the newest arguments, so
//       dragging a slider sends a handful of calls instead of hundreds.
//
//   host.on(eventName, fn)            -> unsubscribe()
//       Python -> JS pushes (Python: events.emit("eventName", data)).
//
// Every Python method is documented in src/api.py / src/backend/engine.py.
// ---------------------------------------------
(function () {
  const listeners = new Map();   // event name -> Set<fn>
  const channels = new Map();    // channel name -> { items: [], busy: bool }

  let api = null;
  let resolveReady;
  const ready = new Promise((resolve) => { resolveReady = resolve; });

  function checkReady() {
    if (api) return true;
    const candidate = window.pywebview && window.pywebview.api;
    if (candidate && Object.keys(candidate).length > 0) {
      api = candidate;
      dbg("pywebview bridge ready. api methods:", Object.keys(api));
      resolveReady(api);
      return true;
    }
    return false;
  }

  window.addEventListener("pywebviewready", checkReady);

  // Safety net in case the event fired before this script ran.
  if (!checkReady()) {
    let polls = 0;
    const timer = setInterval(() => {
      if (checkReady()) {
        clearInterval(timer);
      } else if (++polls > 200) {  // ~10 s
        clearInterval(timer);
        dbgErr("host-bridge: pywebview API never became available");
      }
    }, 50);
  }

  async function call(name, ...args) {
    try {
      const a = await ready;
      const fn = a[name];
      if (typeof fn !== "function") {
        throw new Error("pywebview.api." + name + " is not a function");
      }
      dbg("host.call ->", name, args);
      const result = await fn(...args);
      dbg("host.call <-", name, result);
      return result;
    } catch (e) {
      dbgErr("host.call failed:", name, e);
      return { success: false, error: (e && e.message) || String(e) };
    }
  }

  // ---- ordered channels ----------------------------------------------

  function enqueue(channelName, name, args, latest) {
    let ch = channels.get(channelName);
    if (!ch) {
      ch = { items: [], busy: false };
      channels.set(channelName, ch);
    }

    return new Promise((resolve) => {
      const tail = ch.items[ch.items.length - 1];
      if (latest && tail && tail.latest && tail.name === name) {
        tail.args = args;              // newer arguments replace the waiting ones
        tail.waiters.push(resolve);
      } else {
        ch.items.push({ name, args, latest, waiters: [resolve] });
      }
      if (!ch.busy) pump(ch);
    });
  }

  async function pump(ch) {
    ch.busy = true;
    while (ch.items.length) {
      const item = ch.items.shift();
      const result = await call(item.name, ...item.args);
      item.waiters.forEach((resolve) => resolve(result));
    }
    ch.busy = false;
  }

  // ---- Python -> JS events -------------------------------------------

  function on(eventName, fn) {
    let set = listeners.get(eventName);
    if (!set) {
      set = new Set();
      listeners.set(eventName, set);
    }
    set.add(fn);
    return () => set.delete(fn);
  }

  // Called by Python via window.evaluate_js (see backend/events.py).
  function _emit(eventName, data) {
    dbg("host event:", eventName, data);
    const set = listeners.get(eventName);
    if (!set) return;
    set.forEach((fn) => {
      try {
        fn(data);
      } catch (e) {
        dbgErr("host.on handler for", eventName, "threw", e);
      }
    });
  }

  window.host = {
    call,
    queue: (channel, name, ...args) => enqueue(channel, name, args, false),
    queueLatest: (channel, name, ...args) => enqueue(channel, name, args, true),
    on,
    ready,
    _emit,
  };

  dbg("host-bridge loaded. API present now:", !!api);
})();
