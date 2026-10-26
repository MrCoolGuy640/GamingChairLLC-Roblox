// ---------------------------------------------
// Explorer: object tree + properties panel
//
// Backend node shape (see backend/explorer.rs):
//   { address: "0x1234", name: "Workspace", className: "Workspace", hasChildren: true }
// Lookahead format: { "<decimal address>": [childNodes...] }
//
// The tree keeps its state on purpose:
//   * `expandedNodes` remembers which branches the user opened.
//   * `selectedNodeAddress` remembers the focused object.
// Navigating to another page and back therefore shows the exact same view
// instead of rebuilding a collapsed tree. A refresh still runs in the
// background and swaps in the new tree atomically, so if the focused object
// (or one of its parents) was removed in-game the focus is dropped in that same
// update - the user never sees a stale focus get collapsed a moment later.
// When nothing changed the swap is skipped entirely (the existing DOM is kept),
// so there is no flicker, no icon reload and no scroll jump.
// ---------------------------------------------
const chevronPath = '<path d="M9 6l6 6-6 6"/>';
const ICON_BASE = "/icons/roblox/whitescale";   // served by the local resource server

let selectedNodeAddress = null;

// Hex address -> true for every branch that is currently expanded.
const expandedNodes = new Set();

// Hex address -> parent address (`null` for a top-level service). Filled in
// while rendering. A refresh uses it to walk up from the focused object and
// find out whether it still exists, without confusing "removed in-game" with
// "inside a branch the user collapsed".
const parentByAddress = new Map();

// Prevent overlapping refreshes; the newest request wins.
let explorerLoading = false;
let explorerRefreshQueued = false;
// Bumped on detach so a reply for the previous session is discarded.
let explorerEpoch = 0;
// Bumped whenever the user changes what is expanded. A refresh that was
// already in flight then has a stale snapshot; it is discarded instead of
// being rendered (which would undo the user's click).
let explorerRevision = 0;
// Exact snapshot the current DOM was built from, plus its structural
// signature. Used to skip a pointless (and potentially flickering) swap on
// every page switch.
let renderedState = null;
let renderedTreeKey = null;

function clearProperties() {
  document.getElementById("propertiesBody").innerHTML =
    '<div class="properties-empty">Select an object to view its properties.</div>';
}

function showProperties(props) {
  const body = document.getElementById("propertiesBody");
  body.innerHTML = "";

  if (!props || Object.keys(props).length === 0) {
    body.innerHTML = '<div class="properties-empty">No attributes.</div>';
    return;
  }

  const frag = document.createDocumentFragment();
  Object.entries(props).forEach(([key, value]) => {
    const row = document.createElement("div");
    row.className = "property-row";

    const keyEl = document.createElement("span");
    keyEl.className = "property-key";
    keyEl.textContent = key;

    const valEl = document.createElement("span");
    valEl.className = "property-value";
    valEl.textContent = value;

    row.appendChild(keyEl);
    row.appendChild(valEl);
    frag.appendChild(row);
  });
  body.appendChild(frag);
}

// Backend lookahead keys are decimal strings; tree rows use "0x..." strings.
function addressKey(address) {
  if (typeof address !== "string") return "0x" + Number(address).toString(16);
  return address.startsWith("0x") ? address : "0x" + parseInt(address, 10).toString(16);
}

// Merge a backend `lookahead` object into a Map<hexAddress, childNodes[]>.
function mergeLookahead(target, lookahead) {
  if (!lookahead) return;
  for (const [address, children] of Object.entries(lookahead)) {
    target.set(addressKey(address), children || []);
  }
}

// ---------------------------------------------
// Rendering
// ---------------------------------------------

// Render one node (and, if it is expanded, its already-fetched children).
// `lookahead` describes the *siblings* of this node, `childrenMap` holds the
// fetched children of every expanded node for this render pass.
function renderNode(node, depth, lookahead, childrenMap, parentAddress) {
  const address = node.address;
  parentByAddress.set(address, parentAddress);

  const kids = childrenMap.get(address);
  const knownChildren = kids ? kids.nodes : lookahead.get(address) || [];
  const hasChildren = knownChildren.length > 0;
  const isExpanded = hasChildren && expandedNodes.has(address) && !!kids;

  const isService = depth === 0;

  const row = document.createElement("div");
  row.className = "tree-row";
  if (address === selectedNodeAddress) row.classList.add("is-selected");
  row.style.paddingLeft = (depth * 16 + 8) + "px";
  row.dataset.address = address;
  row.dataset.depth = depth;

  const chevron = document.createElement("span");
  chevron.className = "tree-chevron" + (hasChildren ? "" : " is-empty");
  if (isExpanded) chevron.classList.add("is-expanded");
  chevron.innerHTML = hasChildren
    ? `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">${chevronPath}</svg>`
    : "";
  row.appendChild(chevron);

  const icon = document.createElement("span");
  icon.className = "tree-icon";

  const iconFolder = isService ? "services" : "objects";
  const fallback = isService ? "default_service.png" : "default_object.png";
  const iconName = isService ? node.name : (node.className || fallback.replace(".png", ""));

  const img = document.createElement("img");
  img.src = `${ICON_BASE}/${iconFolder}/${encodeURIComponent(iconName)}.png`;
  img.alt = "";
  img.draggable = false;
  img.style.width = "16px";
  img.style.height = "16px";
  img.style.objectFit = "contain";
  img.style.display = "block";
  img.onerror = () => {
    img.onerror = null;
    img.src = `${ICON_BASE}/${fallback}`;
  };

  icon.appendChild(img);
  row.appendChild(icon);

  const label = document.createElement("span");
  label.className = "tree-label";
  label.textContent = node.name;
  row.appendChild(label);

  const typeLabel = document.createElement("span");
  typeLabel.className = "tree-type";
  typeLabel.textContent = node.className;
  row.appendChild(typeLabel);

  const wrapper = document.createElement("div");
  wrapper.className = "tree-node";
  wrapper.appendChild(row);

  let childContainer = null;
  if (hasChildren) {
    childContainer = document.createElement("div");
    childContainer.className = "tree-children";
    wrapper.appendChild(childContainer);

    if (isExpanded) {
      childContainer.style.display = "block";
      const frag = document.createDocumentFragment();
      kids.nodes.forEach((child) => {
        frag.appendChild(
          renderNode(child, depth + 1, kids.lookahead, childrenMap, address).wrapper
        );
      });
      childContainer.appendChild(frag);
      childContainer.dataset.loaded = "true";
    }
  }

  function toggleExpand() {
    if (!hasChildren || !childContainer) return;

    const isOpen = childContainer.style.display === "block";
    if (isOpen) {
      childContainer.style.display = "none";
      chevron.classList.remove("is-expanded");
      expandedNodes.delete(address);
      syncRenderedKey();
      return;
    }

    childContainer.style.display = "block";
    chevron.classList.add("is-expanded");
    expandedNodes.add(address);
    explorerRevision++;
    syncRenderedKey();

    // Children were already fetched during a previous refresh / expand.
    if (childContainer.dataset.loaded === "true") return;

    loadBranch(address, childContainer, depth + 1);
  }

  row.addEventListener("click", (e) => {
    e.stopPropagation();

    const previous = document.querySelector(".tree-row.is-selected");
    if (previous) previous.classList.remove("is-selected");
    row.classList.add("is-selected");
    selectedNodeAddress = address;
    syncRenderedKey();

    host.call("get_properties", address).then(handleProperties);
    toggleExpand();
  });

  row.addEventListener("dblclick", (e) => {
    e.stopPropagation();
    toggleExpand();
  });

  return { wrapper, childContainer };
}

// Load a branch that is being expanded after it was collapsed (or for the
// first time). Expanded descendants are fetched too, so their state survives
// even when the parent branch is rebuilt from scratch.
async function loadBranch(address, childContainer, depth) {
  const descendants = Array.from(expandedNodes).filter(
    (candidate) => candidate !== address && isDescendantOf(candidate, address)
  );

  const childrenMap = await fetchChildrenMap([address, ...descendants]);

  // Collapsed again, detached, or the whole tree was re-rendered before the
  // reply arrived: the new DOM is authoritative.
  if (!childrenMap.has(address)) return;
  if (childContainer.style.display !== "block") return;
  if (!document.body.contains(childContainer)) return;

  populateChildren(childContainer, address, depth, childrenMap);
}

// Fill a branch with the freshly fetched children.
function populateChildren(childContainer, address, depth, childrenMap) {
  const entry = childrenMap.get(address);
  if (!entry) return;

  childContainer.innerHTML = "";
  childContainer.dataset.loaded = "true";

  const frag = document.createDocumentFragment();
  entry.nodes.forEach((node) => {
    frag.appendChild(
      renderNode(node, depth, entry.lookahead, childrenMap, address).wrapper
    );
  });
  childContainer.appendChild(frag);

  // Keep the snapshot in sync with the DOM so a later refresh can recognise
  // that the tree already shows this state and skip a re-render.
  if (renderedState) {
    childrenMap.forEach((value, key) => renderedState.children.set(key, value));
    syncRenderedKey();
  }
}

// Walk the known parent chain and return false only when we have proof the
// object is gone. "The parent's children were fetched and do not contain it"
// is proof. "The parent was not fetched because the user collapsed it" is not.
function nodeStillExists(address, state) {
  let current = address;

  for (;;) {
    if (!parentByAddress.has(current)) return true;
    const parent = parentByAddress.get(current);

    let siblings;
    if (parent === null) {
      siblings = state.nodes;
    } else if (state.children.has(parent)) {
      siblings = state.children.get(parent).nodes;
    } else {
      return true;
    }

    if (!siblings.some((node) => node.address === current)) return false;
    if (parent === null) return true;
    current = parent;
  }
}

function isDescendantOf(address, ancestor) {
  let current = address;
  let guard = 0;

  while (parentByAddress.has(current) && guard++ < 64) {
    const parent = parentByAddress.get(current);
    if (parent === null) return false;
    if (parent === ancestor) return true;
    current = parent;
  }
  return false;
}

// Forget expansion state for branches that no longer exist, so we do not keep
// querying dead addresses on every refresh.
function pruneExpandedNodes(state) {
  for (const address of Array.from(expandedNodes)) {
    if (!nodeStillExists(address, state)) expandedNodes.delete(address);
  }
}

// ---------------------------------------------
// Fetching + atomic refresh
// ---------------------------------------------

// children of the given addresses -> Map<address, { nodes, lookahead }>.
async function fetchChildrenMap(addresses) {
  const replies = await Promise.all(
    addresses.map((address) => host.call("get_explorer_children", address))
  );

  const children = new Map();
  replies.forEach((reply, index) => {
    if (!reply || !Array.isArray(reply.nodes)) return;

    const lookahead = new Map();
    mergeLookahead(lookahead, reply.lookahead);
    children.set(addresses[index], { nodes: reply.nodes, lookahead });
  });
  return children;
}

// Fetch the root list plus the children of every expanded branch. Doing this
// before touching the DOM means the view is swapped in one go instead of
// showing a collapsed tree that "grows" over several IPC replies.
async function fetchExplorerState() {
  const root = await host.call("get_explorer_tree");
  if (!root || !Array.isArray(root.nodes)) return null;

  const rootLookahead = new Map();
  mergeLookahead(rootLookahead, root.lookahead);

  const children = await fetchChildrenMap(Array.from(expandedNodes));

  return { nodes: root.nodes, rootLookahead, children };
}

// A stable description of everything the tree renders. If it did not change,
// the DOM already shows the right thing (no need to replace it).
function treeKey(state) {
  const nodeKey = (node) => [node.address, node.name, node.className];
  const levelKey = (level) =>
    Array.from(level.entries())
      .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
      .map(([address, nodes]) => [address, nodes.map(nodeKey)]);
  // Only expanded branches are rendered, so only those can affect the DOM.
  // Collapsed subtrees may still be cached in the DOM; ignore them.
  const childrenKey = Array.from(state.children.entries())
    .filter(([address]) => expandedNodes.has(address))
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([address, entry]) => [
      address,
      entry.nodes.map(nodeKey),
      levelKey(entry.lookahead),
    ]);

  return JSON.stringify({
    roots: state.nodes.map(nodeKey),
    lookahead: levelKey(state.rootLookahead),
    children: childrenKey,
    expanded: Array.from(expandedNodes).sort(),
    selected: selectedNodeAddress,
  });
}

function syncRenderedKey() {
  renderedTreeKey = renderedState ? treeKey(renderedState) : null;
}

// Swap in a fully-built tree. Everything is preserved unless the game no
// longer contains the focused object, in which case the focus is dropped in
// this same update (never a moment later).
function renderExplorer(state) {
  const tree = document.getElementById("objectTree");

  pruneExpandedNodes(state);

  if (selectedNodeAddress && !nodeStillExists(selectedNodeAddress, state)) {
    // The focused object, or one of its parents, is gone. Clear it before the
    // swap so the new tree and the properties panel agree in the same frame.
    selectedNodeAddress = null;
    clearProperties();
  }

  const key = treeKey(state);

  // Nothing changed while the explorer was hidden: keep the live DOM (and its
  // loaded icons, scroll position and handlers) exactly as it is.
  if (key === renderedTreeKey) {
    if (selectedNodeAddress) {
      host.call("get_properties", selectedNodeAddress).then(handleProperties);
    }
    return;
  }

  const scrollTop = tree.scrollTop;

  const staging = document.createElement("div");
  state.nodes.forEach((node) => {
    staging.appendChild(
      renderNode(node, 0, state.rootLookahead, state.children, null).wrapper
    );
  });
  tree.replaceChildren(...staging.children);
  tree.scrollTop = scrollTop;

  renderedState = state;
  renderedTreeKey = key;

  if (selectedNodeAddress) {
    // Same object, but its values may have changed while we were away.
    host.call("get_properties", selectedNodeAddress).then(handleProperties);
  }
}

async function refreshExplorer() {
  if (!isAttached) return;

  if (explorerLoading) {
    explorerRefreshQueued = true;
    return;
  }

  const epoch = explorerEpoch;
  const revision = explorerRevision;
  explorerLoading = true;
  try {
    const state = await fetchExplorerState();
    if (!state || !isAttached || epoch !== explorerEpoch) return;

    if (revision !== explorerRevision) {
      // The user expanded / collapsed something while we were fetching, so
      // this snapshot would undo that click. Fetch again instead of rendering.
      explorerRefreshQueued = true;
      return;
    }

    renderExplorer(state);
  } finally {
    explorerLoading = false;
    if (explorerRefreshQueued) {
      explorerRefreshQueued = false;
      refreshExplorer();
    }
  }
}

function handleProperties(result) {
  if (!result || !result.properties) {
    dbgWarn("handleProperties: no properties field", result);
    return;
  }
  // A slow reply for an object the user has already clicked away from.
  if (result.address !== undefined && result.address !== selectedNodeAddress) return;
  showProperties(result.properties);
}

function loadExplorerTree() {
  refreshExplorer();
}

function clearExplorerTree() {
  document.getElementById("objectTree").replaceChildren();
  clearProperties();
  selectedNodeAddress = null;
  expandedNodes.clear();
  parentByAddress.clear();
  explorerEpoch++;
  explorerRevision++;
  explorerRefreshQueued = false;
  renderedState = null;
  renderedTreeKey = null;
}

// Search box (debounced)
const filterInput = document.querySelector(".filter-input");
if (filterInput) {
  let searchTimeout = null;
  filterInput.addEventListener("input", () => {
    clearTimeout(searchTimeout);
    searchTimeout = setTimeout(() => {
      const query = filterInput.value.trim();
      if (query.length > 0) {
        host.call("search_explorer", query).then((result) => {
          // TODO: show the matches in the tree. For now they're only logged.
          dbg("Search results:", result && result.results);
        });
      }
    }, 300);
  });
}
