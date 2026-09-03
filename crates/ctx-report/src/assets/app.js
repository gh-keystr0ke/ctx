(() => {
  "use strict";

  const searchData = document.getElementById("search-data");
  const searchLinks = document.getElementById("search-links");
  const searchInput = document.getElementById("tree-search");
  const searchResults = document.getElementById("search-results");
  if (searchData && searchLinks && searchInput && searchResults) {
    const entries = JSON.parse(searchData.textContent);
    const links = JSON.parse(searchLinks.textContent);
    const renderSearch = () => {
      const query = searchInput.value.trim().toLocaleLowerCase();
      searchResults.replaceChildren();
      if (!query) return;
      const fragment = document.createDocumentFragment();
      for (const entry of entries) {
        const haystack = [entry.name, entry.identifier, entry.kind, entry.file_path || ""]
          .join("\n")
          .toLocaleLowerCase();
        if (!haystack.includes(query)) continue;
        const item = document.createElement("li");
        const label = links[entry.stable_key]
          ? document.createElement("a")
          : document.createElement("span");
        label.textContent = `${entry.name} · ${entry.kind}`;
        if (links[entry.stable_key]) label.href = links[entry.stable_key];
        const identifier = document.createElement("code");
        identifier.textContent = entry.identifier;
        item.append(label, identifier);
        fragment.append(item);
        if (fragment.childNodes.length >= 100) break;
      }
      searchResults.append(fragment);
    };
    searchInput.addEventListener("input", renderSearch);
  }

  const canvas = document.getElementById("graph-canvas");
  const graphData = document.getElementById("graph-data");
  const graphLinks = document.getElementById("graph-links");
  if (!canvas || !graphData || !graphLinks) return;
  const graph = JSON.parse(graphData.textContent);
  const links = JSON.parse(graphLinks.textContent);
  const context = canvas.getContext("2d");
  const ratio = window.devicePixelRatio || 1;
  const nodesByKey = new Map(graph.nodes.map((node) => [node.stable_key, node]));
  const enabledKinds = new Set(graph.nodes.map((node) => node.kind));
  const positions = new Map();
  const view = { x: 0, y: 0, scale: 1 };
  let viewport = { width: 1, height: 1 };
  let drag = null;
  graph.nodes.forEach((node, index) => {
    const angle = (index / Math.max(1, graph.nodes.length)) * Math.PI * 2;
    const ring = 110 + (index % 8) * 34;
    positions.set(node.stable_key, [Math.cos(angle) * ring, Math.sin(angle) * ring]);
  });
  const resize = () => {
    const bounds = canvas.getBoundingClientRect();
    canvas.width = Math.max(1, bounds.width * ratio);
    canvas.height = Math.max(1, bounds.height * ratio);
    viewport = { width: bounds.width, height: bounds.height };
    draw();
  };
  const draw = () => {
    context.setTransform(ratio, 0, 0, ratio, 0, 0);
    context.clearRect(0, 0, viewport.width, viewport.height);
    context.translate(viewport.width / 2 + view.x, viewport.height / 2 + view.y);
    context.scale(view.scale, view.scale);
    context.strokeStyle = "rgba(116, 146, 168, .24)";
    graph.edges.forEach((edge) => {
      const sourceNode = nodesByKey.get(edge.source);
      const targetNode = nodesByKey.get(edge.target);
      if (!enabledKinds.has(sourceNode?.kind) || !enabledKinds.has(targetNode?.kind)) return;
      const source = positions.get(edge.source);
      const target = positions.get(edge.target);
      if (!source || !target) return;
      context.beginPath();
      context.moveTo(source[0], source[1]);
      context.lineTo(target[0], target[1]);
      context.stroke();
    });
    graph.nodes.forEach((node) => {
      if (!enabledKinds.has(node.kind)) return;
      const [x, y] = positions.get(node.stable_key);
      context.fillStyle = "#6de5bd";
      context.beginPath();
      context.arc(x, y, 3.5, 0, Math.PI * 2);
      context.fill();
    });
  };
  document.querySelectorAll("#graph-filters [data-kind]").forEach((button) => {
    button.addEventListener("click", () => {
      const kind = button.dataset.kind;
      if (enabledKinds.has(kind)) enabledKinds.delete(kind);
      else enabledKinds.add(kind);
      button.setAttribute("aria-pressed", String(enabledKinds.has(kind)));
      draw();
    });
  });
  canvas.addEventListener("pointerdown", (event) => {
    drag = { x: event.clientX, y: event.clientY, originX: view.x, originY: view.y };
    canvas.setPointerCapture(event.pointerId);
  });
  canvas.addEventListener("pointermove", (event) => {
    if (!drag) return;
    view.x = drag.originX + event.clientX - drag.x;
    view.y = drag.originY + event.clientY - drag.y;
    draw();
  });
  canvas.addEventListener("pointerup", (event) => {
    if (drag && Math.hypot(event.clientX - drag.x, event.clientY - drag.y) < 5) {
      const bounds = canvas.getBoundingClientRect();
      const x = (event.clientX - bounds.left - viewport.width / 2 - view.x) / view.scale;
      const y = (event.clientY - bounds.top - viewport.height / 2 - view.y) / view.scale;
      const selected = graph.nodes.find((node) => {
        if (!enabledKinds.has(node.kind)) return false;
        const position = positions.get(node.stable_key);
        return Math.hypot(position[0] - x, position[1] - y) <= 9;
      });
      if (selected && links[selected.stable_key]) window.location.href = links[selected.stable_key];
    }
    drag = null;
    canvas.releasePointerCapture(event.pointerId);
  });
  canvas.addEventListener("wheel", (event) => {
    event.preventDefault();
    view.scale = Math.min(4, Math.max(.3, view.scale * (event.deltaY < 0 ? 1.12 : .89)));
    draw();
  }, { passive: false });
  new ResizeObserver(resize).observe(canvas);
  resize();
})();
