"use strict";
// Presentation only. Source access stays in app.js and its guarded callbacks.
(() => {
  const get = id => document.getElementById(id);
  const text = (id, value) => { if (get(id)) get(id).textContent = value; };
  const hide = (id, value) => { if (get(id)) get(id).hidden = value; };
  const make = (tag, value) => { const node = document.createElement(tag); if (value !== undefined) node.textContent = value; return node; };
  const describe = value => typeof value === "string" ? value : JSON.stringify(value, null, 2);
  const views = {sequence:"sequence-panel", classes:"classes-panel", libraries:"dependency-library", tools:"tools-panel"};
  let openSource = null, sourceReturnFocus = null, activeView = "sequence";
  function drawer(name, open) {
    if (open && window.matchMedia?.("(max-width: 900px)").matches) drawer(name === "explorer" ? "inspector" : "explorer", false);
    document.body.classList.toggle(`${name}-open`, open);
    get(`${name}-toggle`)?.setAttribute("aria-expanded", String(open));
  }
  function showView(name) {
    if (!Object.hasOwn(views, name)) return;
    activeView = name;
    document.body.classList.toggle("classes-view", name === "classes");
    window.BaleygClasses?.closeContextMenu?.();
    if (name === "sequence" || name === "classes") drawer("explorer", false);
    for (const [key, panel] of Object.entries(views)) {
      hide(panel, key !== name);
      const tab = get(`view-${key}`);
      tab?.setAttribute("aria-selected", String(key === name));
      if (tab) tab.tabIndex = key === name ? 0 : -1;
    }
  }
  function showSource(name) {
    if (!["workspace", "library"].includes(name)) return;
    if (!get("source-dock")?.contains?.(document.activeElement)) sourceReturnFocus = document.activeElement;
    hide("source-dock", false);
    for (const key of ["workspace", "library"]) {
      hide(`${key}-source-panel`, key !== name);
      get(`dock-${key}`)?.setAttribute("aria-selected", String(key === name));
      if (get(`dock-${key}`)) get(`dock-${key}`).tabIndex = key === name ? 0 : -1;
    }
    // Clear latent mobile drawers even on desktop: a later resize must not
    // place an old inspector/explorer over the open source dock.
    drawer("explorer", false); drawer("inspector", false);
    if (window.matchMedia?.("(max-width: 900px)").matches) {
      get(`dock-${name}`)?.focus();
    }
  }
  function visibleControl(node) {
    return node && !node.disabled && !node.hidden && !node.closest?.("[hidden]") && (!node.getClientRects || node.getClientRects().length > 0);
  }
  function closeSource() {
    hide("source-dock", true);
    const target = visibleControl(sourceReturnFocus) ? sourceReturnFocus : get(`view-${activeView}`);
    target?.focus(); sourceReturnFocus = null;
  }
  function wireTabs(names, prefix, select) {
    names.forEach((name, index) => {
      const tab = get(prefix + name);
      tab?.addEventListener("click", () => select(name));
      tab?.addEventListener("keydown", event => {
        let next;
        if (event.key === "ArrowRight") next = (index + 1) % names.length;
        else if (event.key === "ArrowLeft") next = (index + names.length - 1) % names.length;
        else if (event.key === "Home") next = 0;
        else if (event.key === "End") next = names.length - 1;
        else return;
        event.preventDefault(); select(names[next]); get(prefix + names[next])?.focus();
      });
    });
  }
  function resetInspector() {
    openSource = null;
    drawer("inspector", false);
    hide("inspector-empty", false); hide("inspector-content", true);
    for (const id of ["inspector-title", "inspector-kind", "inspector-target", "inspector-location", "inspector-evidence", "inspector-detail"]) get(id)?.replaceChildren();
    if (get("inspector-open-source")) get("inspector-open-source").disabled = true;
    if (get("inspector-clear")) get("inspector-clear").disabled = true;
    if (get("inspector-title")) get("inspector-title").title = "";
  }
  function participant(view, target) { return (view.participants || []).find(item => item.id === target); }
  function targetText(step, view, full = true) {
    const target = participant(view, step.target);
    if (!target) return step.target ? `Target reference: ${step.target}` : "No target identified for this step.";
    const labels = {externalCandidate:"External candidate, not resolved dispatch", unresolvedReceiver:"Receiver type unresolved", unresolvedCallee:"Callee unresolved", boundary:"Unknown target", builtin:"Built-in name, not runtime proof", import:"Imported binding, not runtime proof", receiver:"Receiver hint", internal:"Indexed target", method:"Selected method"};
    return `${target.label} · ${labels[target.kind] || target.kind}${full && target.identification ? " · " + target.identification : ""}`;
  }
  function location(step) {
    if (!step.path || !step.range) return "Source range unavailable";
    const r = step.range;
    return `${step.path}:${r.startLine}${r.startColumn == null ? "" : ":" + r.startColumn}–${r.endLine}${r.endColumn == null ? "" : ":" + r.endColumn}`;
  }
  function selectStep(step, view, callback) {
    resetInspector();
    if (!step || !view) return;
    hide("inspector-empty", true); hide("inspector-content", false);
    if (get("inspector-clear")) get("inspector-clear").disabled = false;
    text("inspector-title", step.label || step.id || "Selected step");
    text("inspector-kind", step.kind || "Step");
    text("inspector-location", `${location(step)} · revision ${view.revision}`);
    const children = step.children || [];
    const flat = step.kind === "group" && !step.alternate?.length && children.length && children.every(child => child.kind === "call" && !child.hidden && !child.children?.length && !child.alternate?.length);
    const entry = flat ? children[0] : step;
    if (flat) text("inspector-title", `${entry.label || entry.id} · +${children.length - 1} chain calls`);
    if (get("inspector-title")) get("inspector-title").title = step.label || step.id || "";
    text("inspector-target", flat
      ? `First measured entry call only: ${targetText(entry, view, false)}. Other calls may have different receivers and return types.`
      : targetText(step, view, false));
    text("inspector-evidence", `Static possible path, not a runtime trace. ${entry.resolution == null ? "No resolution evidence supplied." : "Original resolution: " + describe(entry.resolution)}`);
    const detail = get("inspector-detail");
    // Every original field and all nested guards, alternates and chain calls remain
    // available as safe text. Never infer equivalent guards or invent confidence.
    function evidence(item, parent, label) {
      const disclosure = make("details");
      disclosure.append(make("summary", `${label ? label + " · " : ""}${item.kind || "step"}: ${item.label || item.id || "Details"}`));
      disclosure.append(make("p", `${location(item)} · ${targetText(item, view)}`));
      const fields = Object.fromEntries(Object.entries(item).filter(([key]) => !["children", "alternate"].includes(key)));
      disclosure.append(make("pre", JSON.stringify(fields, null, 2)));
      for (const child of item.children || []) evidence(child, disclosure, "Child");
      for (const child of item.alternate || []) evidence(child, disclosure, "Alternate");
      parent.append(disclosure);
      return disclosure;
    }
    if (detail) evidence(step, detail, "Original evidence");
    openSource = step.path && step.range && typeof callback === "function" ? callback : null;
    if (get("inspector-open-source")) get("inspector-open-source").disabled = !openSource;
    drawer("inspector", true);
  }
  function updateWorkspace(status) {
    const root = status?.workspaceRoot || "";
    text("workspace-name", root.split(/[\\/]/).filter(Boolean).at(-1) || "No workspace");
    text("workspace-meta", status ? `Revision ${status.revision} · ${status.stats?.files ?? 0} files · ${status.stats?.semanticState === "unavailable" ? "Syntax only" : status.stats?.semanticState || "syntax evidence"}` : "Connect to a local daemon");
    if (get("workspace-name")) get("workspace-name").title = root;
  }
  function reset() {
    resetInspector(); showView("sequence"); hide("source-dock", true); sourceReturnFocus = null;
    drawer("explorer", false); drawer("inspector", false); updateWorkspace(null);
  }
  function setConnected(connected) {
    document.body.classList.toggle("connected", !!connected);
    if (!connected) reset();
  }
  wireTabs(Object.keys(views), "view-", showView);
  wireTabs(["workspace", "library"], "dock-", showSource);
  get("inspector-open-source")?.addEventListener("click", () => { if (openSource) return openSource(); });
  get("inspector-clear")?.addEventListener("click", resetInspector);
  get("source-dock-close")?.addEventListener("click", closeSource);
  for (const name of ["explorer", "inspector"]) get(`${name}-toggle`)?.addEventListener("click", () => drawer(name, !document.body.classList.contains(`${name}-open`)));
  document.addEventListener("keydown", event => {
    if (event.key !== "Escape") return;
    const mobile = window.matchMedia?.("(max-width: 900px)").matches;
    if (mobile && document.body.classList.contains("inspector-open")) { drawer("inspector", false); get("inspector-toggle")?.focus(); }
    else if (mobile && document.body.classList.contains("explorer-open")) { drawer("explorer", false); get("explorer-toggle")?.focus(); }
    else if (get("source-dock") && !get("source-dock").hidden) closeSource();
  });
  window.BaleygShell = {setConnected, updateWorkspace, showView, showSource, closeSource, selectStep, resetInspector, reset};
  reset();
})();
