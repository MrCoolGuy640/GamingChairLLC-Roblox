// ---------------------------------------------
// Explorer: object tree + properties panel
//
// Python (Engine.get_explorer_tree / get_explorer_children / get_properties)
// returns nodes shaped like:
//   { address: "0x1234", name: "Workspace", className: "Workspace", hasChildren: true }
// Lookahead format: { "0x1234": [childNodes...] } for hasChildren of displayed nodes
// ---------------------------------------------
const chevronPath = '<path d="M9 6l6 6-6 6"/>';
const ICON_BASE = "/icons/roblox/whitescale";   // served by the local resource server

let selectedNodeAddress = null;

// Lookahead cache: address -> hasChildren (boolean)
const lookaheadCache = new Map();

// Icon cache: tracks which icons exist/missing to avoid repeated 404s
const iconCache = new Map(); // key: "folder/iconName" -> true (exists) | false (missing)

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

function createTreeNode(node, depth) {
  // hasChildren comes from lookahead cache (set by backend), not from node.hasChildren
  const lookaheadEntry = lookaheadCache.get(node.address);
  const hasChildren = lookaheadEntry && lookaheadEntry.length > 0;
  
  // Debug
  if (!hasChildren && lookaheadCache.size > 0) {
    console.log("[DEBUG] Node:", node.address, node.name, "hasChildren:", hasChildren, "lookaheadEntry:", lookaheadEntry);
    console.log("[DEBUG] lookaheadCache keys:", Array.from(lookaheadCache.keys()).slice(0, 10));
  }
  
  const isService = depth === 0;

  const row = document.createElement("div");
  row.className = "tree-row";
  row.style.paddingLeft = (depth * 16 + 8) + "px";
  row.dataset.address = node.address;
  row.dataset.depth = depth;

  const chevron = document.createElement("span");
  chevron.className = "tree-chevron" + (hasChildren ? "" : " is-empty");
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
    childContainer.style.display = "none";
    wrapper.appendChild(childContainer);
  }

  function toggleExpand() {
    if (!hasChildren) return;
    const isOpen = childContainer.style.display === "block";
    if (!isOpen) {
      host.call("get_explorer_children", node.address).then(handleExplorerChildren);
    }
    childContainer.style.display = isOpen ? "none" : "block";
    chevron.classList.toggle("is-expanded", !isOpen);
  }

  row.addEventListener("click", (e) => {
    e.stopPropagation();

    const previous = document.querySelector(".tree-row.is-selected");
    if (previous) previous.classList.remove("is-selected");
    row.classList.add("is-selected");
    selectedNodeAddress = node.address;

    host.call("get_properties", node.address).then(handleProperties);
    toggleExpand();
  });

  row.addEventListener("dblclick", (e) => {
    e.stopPropagation();
    toggleExpand();
  });

  return { wrapper, childContainer };
}

function handleExplorerTree(result) {
  const objectTree = document.getElementById("objectTree");
  objectTree.innerHTML = "";

  if (!result || !Array.isArray(result.nodes)) {
    dbgWarn("handleExplorerTree: no nodes array", result);
    return;
  }

  // Update lookahead cache from backend
  if (result.lookahead) {
    lookaheadCache.clear();
    for (const [addr, children] of Object.entries(result.lookahead)) {
      // Backend returns numeric addresses as strings (decimal), convert to hex string
      const hexAddr = addr.startsWith('0x') ? addr : '0x' + parseInt(addr, 10).toString(16);
      lookaheadCache.set(hexAddr, children || []);
    }
  }

  const frag = document.createDocumentFragment();
  result.nodes.forEach((node) => frag.appendChild(createTreeNode(node, 0).wrapper));
  objectTree.appendChild(frag);
}

function handleExplorerChildren(result) {
  if (!result || !Array.isArray(result.nodes)) return;

  const parentRow = document.querySelector(
    `.tree-row[data-address="${CSS.escape(String(result.address))}"]`
  );
  if (!parentRow) {
    dbgWarn("No parent row found for address:", result.address);
    return;
  }

  const childContainer = parentRow.parentElement.querySelector(".tree-children");
  if (!childContainer) return;

  const childDepth = (parseInt(parentRow.dataset.depth, 10) || 0) + 1;
  childContainer.innerHTML = "";

  // Update lookahead cache for the newly displayed nodes
  if (result.lookahead) {
    for (const [addr, children] of Object.entries(result.lookahead)) {
      const hexAddr = addr.startsWith('0x') ? addr : '0x' + parseInt(addr, 10).toString(16);
      lookaheadCache.set(hexAddr, children || []);
    }
  }

  const frag = document.createDocumentFragment();
  result.nodes.forEach((node) => frag.appendChild(createTreeNode(node, childDepth).wrapper));
  childContainer.appendChild(frag);
}

function handleExplorerSearch(result) {
  // TODO: show the matches in the tree. For now they're only logged.
  dbg("Search results:", result && result.results);
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
  if (!isAttached) return;
  host.call("get_explorer_tree").then(handleExplorerTree);
}

function clearExplorerTree() {
  document.getElementById("objectTree").innerHTML = "";
  document.getElementById("propertiesBody").innerHTML =
    '<div class="properties-empty">Select an object to view its properties.</div>';
  selectedNodeAddress = null;
  lookaheadCache.clear();
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
        host.call("search_explorer", query).then(handleExplorerSearch);
      }
    }, 300);
  });
}