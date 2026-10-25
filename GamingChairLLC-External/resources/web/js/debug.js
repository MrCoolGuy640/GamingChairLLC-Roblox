// ---------------------------------------------
// DEBUG HELPERS
// ---------------------------------------------
const DEBUG = true;

function dbg(...args) {
  if (DEBUG) console.log("[DEBUG]", ...args);
}
function dbgWarn(...args) {
  if (DEBUG) console.warn("[DEBUG]", ...args);
}
function dbgErr(...args) {
  if (DEBUG) console.error("[DEBUG]", ...args);
}