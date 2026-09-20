"use strict";
// Language-neutral DTO renderer. Geometry never infers calls or control flow.
function renderSequence(container, view, readSource, expandedGroups = new Set(), options = {}) {
  return drawSequence(container, view, readSource, expandedGroups, options, {id:null});
}

function drawSequence(container, view, readSource, expandedGroups, options, selection) {
  const groupControls = new Map(), rowControls = [];
  const ns = "http://www.w3.org/2000/svg";
  const make = (tag, attrs = {}, text) => {
    const n = document.createElementNS(ns, tag);
    for (const [key, value] of Object.entries(attrs)) n.setAttribute(key, String(value));
    if (text !== undefined) n.textContent = text;
    return n;
  };
  container.replaceChildren();
  const allParticipants = view.participants || [];
  const participantById = new Map(allParticipants.map(p => [p.id, p]));
  // A collapsed chain previews its first measured call, never a synthesized
  // call to an inferred common receiver. Later calls can return other types.
  const entryCall = step => {
    const children = step.children || [];
    if (step.kind !== "group" || step.alternate?.length || !children.length ||
        !children.every(child => child.kind === "call" && !child.hidden &&
          !child.children?.length && !child.alternate?.length)) return null;
    return participantById.has(children[0].target) ? children[0] : null;
  };
  const visibleTargets = new Set([view.seed.id]);
  const pending = (view.steps || []).map(step => ({step, depth:0})).reverse();
  let visibleDepth = 0;
  while (pending.length) {
    const {step, depth} = pending.pop();
    if (step.hidden) continue;
    visibleDepth = Math.max(visibleDepth, depth);
    if (step.kind === "group" && !expandedGroups.has(step.id)) {
      const entry = entryCall(step);
      if (entry) visibleTargets.add(entry.target);
      continue;
    }
    if (step.kind === "call") visibleTargets.add(step.target);
    pending.push(...[...(step.alternate || []).slice().reverse(), ...(step.children || []).slice().reverse()].map(step => ({step, depth:depth+1})));
  }
  // Hidden chain members and replaced candidate hints must not leave ghost lanes.
  const participants = [...visibleTargets].map(id => participantById.get(id)).filter(Boolean);
  // Keep a readable guard column even for deeply nested input. The canvas scrolls.
  const width = Math.max(720, participants.length * 210 + 80, visibleDepth * 10 + 400);
  const svg = make("svg", {viewBox:`0 0 ${width} 200`, width, role:"group", "aria-label":"Static possible paths sequence diagram"});
  svg.append(make("title", {}, `${view.seed.name}: static possible paths, not a runtime trace`));
  const positions = new Map(participants.map((p, i) => [p.id, 130 + i * 210]));
  const origin = positions.get(view.seed.id) || 130;
  const lines = make("g", {class:"sequence-lifelines"}); svg.append(lines);
  const kindLabels = {externalCandidate:"External type · candidate", unresolvedReceiver:"Receiver · type unresolved", unresolvedCallee:"Callee · unresolved", boundary:"Unknown target", builtin:"Built-in name", import:"Imported binding", receiver:"Receiver hint", internal:"Indexed target", method:"Selected method"};
  const compactKinds = {externalCandidate:"candidate", unresolvedReceiver:"unresolved", unresolvedCallee:"unresolved", boundary:"unknown", builtin:"source hint", import:"source hint", receiver:"source hint", internal:"indexed", method:"selected"};
  const shorten = (text, limit) => text.length > limit ? `${text.slice(0, limit-1)}…` : text;
  for (const p of participants) {
    const x = positions.get(p.id);
    svg.append(make("rect", {x:x-95, y:10, width:190, height:44, class:"participant"}));
    const text = make("text", {x, y:28, "text-anchor":"middle", class:"sequence-participant-label"}, shorten(p.label, 24));
    text.append(make("title", {}, p.identification ? `${p.label} — ${p.identification}` : p.label)); svg.append(text);
    const meta = make("text", {x, y:44, "text-anchor":"middle", class:"sequence-meta"}, (options.showDetails ? kindLabels[p.kind] : compactKinds[p.kind]) || p.kind);
    meta.append(make("title", {}, [kindLabels[p.kind] || p.kind, p.identification].filter(Boolean).join(" — "))); svg.append(meta);
  }
  // These are evidence categories, never runtime certainty or numeric confidence.
  const provenance = step => {
    const participant = participantById.get(step.target);
    if (participant?.kind === "externalCandidate" || step.resolution === "ambiguous") return {key:"candidate", label:"Candidate", dash:"7 3"};
    if (step.resolution === "internal") return {key:"resolved", label:"Indexed target", dash:"none"};
    return {key:"syntax", label:"Syntax only", dash:"2 4"};
  };
  const tooltip = step => {
    const p = participantById.get(step.target), range = step.range;
    const location = step.path && range ? `${step.path}:${range.startLine}${range.startColumn != null ? ":" + range.startColumn : ""}–${range.endLine}${range.endColumn != null ? ":" + range.endColumn : ""}` : "";
    return [`${step.kind}: ${step.label}`, location,
      step.resolution ? `Resolution: ${JSON.stringify(step.resolution)}` : "",
      p ? `${p.label} — ${p.identification || kindLabels[p.kind] || p.kind}` : ""
    ].filter(Boolean).join(" · ");
  };
  const wrap = (label, limit) => {
    const output = [];
    for (const paragraph of label.split("\n")) {
      let rest = paragraph;
      while (rest.length > limit) {
        const space = rest.lastIndexOf(" ", limit);
        const end = space > limit / 2 ? space : limit;
        output.push(rest.slice(0, end)); rest = rest.slice(end).replace(/^ /, "");
      }
      output.push(rest);
    }
    return output;
  };
  // Exact DTO prose is abbreviated for display only. These mappings never
  // equate guards, alter their source ranges, or change the control tree.
  const compactLabels = new Map([
    ["only if the preceding path continues normally", "continues"],
    ["?: continue only on success; otherwise early return", "success / early return"],
    ["?: early return on residual; conversion not expanded", "early return"],
    ["only if iteration continues normally", "iteration continues"],
    ["arm body only if guard is true", "guard is true"],
    ["RHS only if left is true", "left is true"],
    ["RHS only if left is false", "left is false"],
    ["RHS only if left is truthy", "left is truthy"],
    ["RHS only if left is falsy", "left is falsy"],
    ["RHS only if left is nullish", "left is nullish"],
    ["match: mutually exclusive arms; first matching pattern with true guard", "match · first matching arm"],
    ["body only if condition is true / next item exists (loop: each iteration)", "condition true / next item"],
    ["catch: only if an exception reaches this handler", "catch · exception reaches handler"],
    ["for_expression: possible iterations, not unrolled; exit/termination unknown", "for · possible iterations"],
    ["while_expression: possible iterations, not unrolled; exit/termination unknown", "while · possible iterations"],
    ["loop_expression: possible iterations, not unrolled; exit/termination unknown", "loop · termination unknown"],
    ["Definition boundary: decorators/defaults/annotations and binding effects not expanded; callable body does not execute here.", "definition · effects not expanded"],
    ["Class definition boundary: bases, decorators, metaclass and class-body execution not expanded; method bodies do not execute here.", "class definition · effects not expanded"],
    ["Nested callable boundary: body does not execute at definition; captures are not expanded.", "nested callable · captures not expanded"],
    ["Lambda boundary: body does not execute at definition; captures are not expanded.", "lambda · captures not expanded"],
    ["Nested declaration boundary: bodies are not executed here; implicit initialization is not expanded.", "nested declaration · initialization not expanded"],
    ["Nested function/callback/class boundary: bodies are not executed here; class definition effects (base, computed keys, static initialization) are not expanded.", "nested definition · effects not expanded"],
    ["Attribute lookup: descriptor/__getattribute__ effects and type unresolved.", "attribute lookup · effects/type unresolved"],
    ["Subscription: implicit __getitem__ effects unresolved.", "subscription · effects unresolved"],
    ["Comprehension/generator boundary: iteration, filters and deferred execution not expanded.", "comprehension / generator · not expanded"],
    ["Context-manager/match boundary: implicit protocol and conditional execution not expanded.", "context manager / match · not expanded"],
    ["Async iteration boundary: protocol and suspension not expanded.", "async iteration · protocol not expanded"],
    ["Argument unpacking boundary: iteration/mapping protocol not expanded.", "argument unpacking · protocol not expanded"],
    ["Formatted string boundary: interpolation and formatting protocol not expanded.", "formatted string · protocol not expanded"],
    ["Yield boundary: generator suspension, send/throw/close and yield-from protocol not expanded.", "yield · protocol not expanded"],
    ["Try boundary: exception selection, else and finally execution/transfers not expanded.", "try · paths not expanded"],
    ["Assignment target boundary: unpacking/setter effects not expanded.", "assignment target · effects not expanded"],
    ["Call unpacking boundary: positional/keyword evaluation and iteration effects not expanded.", "call unpacking · effects not expanded"],
    ["Augmented assignment boundary: target read/write and overloaded operator effects not expanded.", "augmented assignment · effects not expanded"],
    ["Chained assignment boundary: target write sequence not expanded.", "chained assignment · writes not expanded"],
    ["Annotation-only target boundary: target address effects not expanded; local annotation is not evaluated.", "annotation target · effects not expanded"],
    ["Chained comparison boundary: later operands are conditional; not expanded.", "chained comparison · conditional / not expanded"],
    ["Loop-else boundary: break versus exhaustion paths not expanded.", "loop else · paths not expanded"],
    ["Async block boundary: future construction does not execute its body; captures are not expanded.", "async block · captures not expanded"],
    ["Opaque macro/unsafe/const boundary: expansion, effects and transfers unknown.", "macro / unsafe / const · effects unknown"],
    ["Unsupported control transfer: remainder of this path omitted.", "unsupported transfer · path omitted"],
    ["Unsupported complex control: possible behavior remains unknown; body not expanded.", "unsupported control · behavior unknown"],
    ["Destructuring binding: computed keys/defaults/implicit reads are not expanded (RHS evaluated first).", "destructuring binding · reads not expanded"],
    ["Destructuring assignment: computed keys/defaults/writes are not expanded (RHS evaluated first).", "destructuring assignment · writes not expanded"],
    ["Loop destructuring binding: per-iteration reads/defaults are not expanded.", "loop destructuring · reads not expanded"],
    ["Parameter defaults/destructuring may execute before the body; behavior not expanded.", "parameter defaults / destructuring · not expanded"],
    ["await: possible suspension/resumption; await protocol unresolved", "await · protocol unresolved"],
    ["await: suspension/resumption boundary, timing unknown", "await · timing unknown"],
    ["iteration target write: implicit setters/unpacking unresolved", "iteration write · setters/unpacking unresolved"]
  ]);
  const quietGuard = step => !options.showDetails && step.kind === "branch" &&
    step.label === "only if the preceding path continues normally" && !step.alternate?.length;
  const analysisKinds = new Set(["effect", "return", "throw", "await", "note", "boundary", "definition"]);
  const compactAnalysis = step => {
    const label = step.label || step.kind;
    if (compactLabels.has(label)) return compactLabels.get(label);
    // Abbreviate DTO evidence categories, not source expressions. Never parse an
    // assignment target or infer a return value from the original source label.
    if (step.kind === "effect") {
      if (label.startsWith("bind name: ")) return `bind ${label.slice("bind name: ".length)}`;
      if (label.startsWith("write: ")) return label.endsWith(" (implicit setters/unpacking unresolved)")
        ? "write · setters/unpacking unresolved" : "write";
      return "effect · inspect details";
    }
    if (step.kind === "return" || step.kind === "throw") return step.kind;
    if (step.kind === "definition") return label.startsWith("define ") ? label : "definition · inspect details";
    return label;
  };
  if (!options.showDetails) svg.append(make("text", {x:24, y:73, class:"sequence-meta"}, "Compact labels · select for details"));
  let y = options.showDetails ? 78 : 98;
  function sourceControl(group, step, x, top, display = step, evidence = step, operator = null) {
    const interactive = typeof options.onSelect === "function" || (step.path && step.range && typeof readSource === "function");
    const call = evidence.kind === "call", proof = call ? provenance(evidence) : null;
    const fullLabel = tooltip(step) + (evidence !== step ? ` · Entry preview only: ${tooltip(evidence)}` : "");
    const control = make("g", {class:"sequence-source", "data-source-step-id":step.id,
      ...(interactive ? {role:"button", tabindex:0, "aria-label":`${options.onSelect ? "Inspect" : "Read source"}: ${fullLabel}`, "aria-pressed":String(selection.id === step.id)} : {})});
    // Compact controls remain individual, ordered fragments. No matching guard
    // text is treated as equivalent, merged, or removed.
    const isControl = ["branch", "loop", "try"].includes(display.kind);
    let label = display.label || display.kind;
    if (options.showDetails) {
      if (!operator && display.kind !== "group" && display.kind !== "call chain" && display.kind !== "call") label = `${display.kind} · ${label}`;
    } else {
      label = analysisKinds.has(display.kind) ? compactAnalysis(display) : (compactLabels.get(label) || label);
      if (quietGuard(display)) label = "[if prior path continues]";
      if (display.kind === "call") label = label.replace(/^call(?:\s*·\s*|\s+|:\s*)/i, "");
    }
    const tabWidth = operator ? Math.max(80, operator.length * 8 + 28) : 0;
    const labelX = operator ? x + tabWidth + 12 : x + 5;
    const capacity = Math.max(24, Math.floor((width-labelX-(proof && options.showDetails ? 145 : 35))/7.8));
    if (!options.showDetails) label = shorten(label.replace(/\s+/g, " "), isControl ? Math.min(56, capacity - (operator ? 2 : 0)) : capacity);
    // Brackets are a presentation frame, not an inferred or rewritten predicate.
    if (operator) label = `[${label}]`;
    let labels = options.showDetails ? wrap(label, capacity) : [label];
    if (display.kind === "call chain" && !options.showDetails) {
      const suffix = ` · +${step.children.length-1} calls collapsed (entry preview)`;
      // Keep the collapsed count and entry-only claim visible even when the
      // measured name is long. At deep indentation the annotation can wrap.
      labels = wrap(shorten(evidence.label, Math.max(12, capacity-suffix.length)) + suffix, capacity);
    }
    const extra = (labels.length-1)*16;
    control.append(make("rect", {x, y:top-(operator ? 19 : 15), width:Math.max(1,width-x-24), height:(operator ? 30 : 24)+extra, fill:"transparent", class:"sequence-row-hit"}));
    if (operator) {
      // UML-style clipped-corner tab. The whole header selects the original step.
      control.append(make("path", {d:`M ${x} ${top-19} H ${x+tabWidth} V ${top+1} L ${x+tabWidth-10} ${top+11} H ${x} Z`, class:"sequence-fragment-tab"}));
      control.append(make("text", {x:x+12, y:top, class:"sequence-fragment-operator"}, operator));
    }
    const text = make("text", {x:labelX, y:top, class:isControl ? "sequence-control-label" : "sequence-label"});
    for (const [i, line] of labels.entries()) text.append(make("tspan", {x:labelX, dy:i ? 16 : 0}, line));
    text.append(make("title", {}, fullLabel)); control.append(text);
    if (proof && options.showDetails) {
      const badge = make("text", {x:width-30, y:top, "text-anchor":"end", class:"sequence-provenance", "data-provenance":proof.key}, proof.label);
      badge.append(make("title", {}, tooltip(evidence))); control.append(badge);
    }
    if (interactive) {
      rowControls.push({control, id:step.id});
      const select = () => {
        selection.id = step.id;
        for (const row of rowControls) row.control.setAttribute("aria-pressed", String(row.id === selection.id));
        if (typeof options.onSelect === "function") options.onSelect(step);
        else readSource(step);
      };
      control.addEventListener("click", select);
      control.addEventListener("keydown", event => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); select(); } });
    }
    group.append(control);
    return extra;
  }
  function drawArrow(group, step) {
    if (!positions.has(step.target)) return;
    const end = positions.get(step.target), proof = provenance(step);
    const attrs = {class:"sequence-arrow", "data-provenance":proof.key, "stroke-dasharray":proof.dash};
    if (end === origin) {
      group.append(make("path", {d:`M ${origin} ${y} h 32 v 14 h -32`, ...attrs}));
      group.append(make("path", {d:`M ${origin+6} ${y+10} L ${origin} ${y+14} L ${origin+6} ${y+18}`, ...attrs, "stroke-dasharray":"none"})); y += 14;
    } else {
      group.append(make("line", {x1:origin, x2:end, y1:y, y2:y, ...attrs}));
      const side = end > origin ? -6 : 6;
      group.append(make("path", {d:`M ${end+side} ${y-4} L ${end} ${y} L ${end+side} ${y+4}`, ...attrs, "stroke-dasharray":"none"}));
    }
  }
  function draw(steps, parent, depth) {
    for (const step of steps || []) {
      if (step.hidden) continue;
      const group = make("g", {"data-kind":step.kind, "data-step-id":step.id}); parent.append(group);
      if (step.kind === "group") {
        const start = y, x = 18 + depth * 10, expanded = expandedGroups.has(step.id);
        const box = make("rect", {x, y:y-19, width:width-x-18, height:34, class:"sequence-fragment"}); group.append(box);
        const control = make("g", {role:"button", tabindex:0, "aria-expanded":String(expanded), "aria-label":`${expanded ? "Collapse" : "Expand"} call chain: ${step.label}`, class:"sequence-group-control"});
        control.append(make("rect", {x:x+4, y:y-15, width:88, height:24, class:"sequence-note"}));
        control.append(make("text", {x:x+10, y}, `${expanded ? "▾ Collapse" : "▸ Expand"}`));
        const toggle = () => {
          const top = container.scrollTop, left = container.scrollLeft;
          if (expandedGroups.has(step.id)) expandedGroups.delete(step.id); else expandedGroups.add(step.id);
          const controls = drawSequence(container, view, readSource, expandedGroups, options, selection);
          controls.get(step.id)?.focus?.({preventScroll:true});
          container.scrollTop = top; container.scrollLeft = left;
        };
        control.addEventListener("click", toggle);
        control.addEventListener("keydown", event => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); toggle(); } });
        groupControls.set(step.id, control); group.append(control);
        const entry = !expanded && entryCall(step);
        if (entry) {
          const preview = make("g", {"data-kind":"call-preview", "data-entry-call-id":entry.callId || entry.id});
          preview.append(make("title", {}, "Preview of the first measured call only. Other calls are collapsed; their receivers and return types may differ."));
          group.append(preview);
          y += sourceControl(preview, step, x+98, y, {kind:"call chain", label:`${entry.label} · +${step.children.length-1} calls collapsed (entry preview)`}, entry);
          y += 16; drawArrow(preview, entry); y += 24;
        } else { y += sourceControl(group, step, x+98, y); y += 28; }
        if (expanded) { draw(step.children, group, depth+1); draw(step.alternate, group, depth+1); y += 4; }
        box.setAttribute("height", y-start+8); y += 8;
        continue;
      }
      const fragment = ["branch", "loop", "try"].includes(step.kind) || step.children?.length || step.alternate?.length;
      if (quietGuard(step)) {
        // One rail per original generated guard. The rail encloses only that
        // guard's children and ends before its next sibling. No flattening.
        const start = y, x = 18 + depth * 10;
        group.setAttribute("data-presentation", "continuation-guard");
        const rail = make("path", {class:"sequence-guard-rail", fill:"none"}); group.append(rail);
        y += sourceControl(group, step, x+10, y); y += 28;
        draw(step.children, group, depth+1);
        rail.setAttribute("d", `M ${x+5} ${start-12} H ${x} V ${y-10} H ${x+5}`);
        y += 8;
      } else if (fragment) {
        const start = y, x = 18 + depth * 10;
        const box = make("rect", {x, y:start-19, width:width-x-18, height:34, class:"sequence-fragment"}); group.append(box);
        const operator = {branch:"alt", loop:"loop", try:"try"}[step.kind] || "block";
        y += sourceControl(group, step, x, y, step, step, operator); y += 34;
        draw(step.children, group, depth+1);
        if (step.alternate?.length) {
          group.append(make("line", {x1:x, x2:width-18, y1:y-12, y2:y-12, class:"sequence-divider"}));
          group.append(make("text", {x:x+10, y:y+2, class:"sequence-meta"}, step.kind === "branch" ? "else / alternate" : "handlers / cleanup (see guards)")); y += 24;
          draw(step.alternate, group, depth+1);
        }
        y += 4; box.setAttribute("height", y-start+8);
        y += 14; // Keep the next fragment's tab clear of this frame's bottom edge.
      } else {
        const note = analysisKinds.has(step.kind);
        if (note && !options.showDetails) group.setAttribute("data-presentation", "quiet-note");
        const labelX = note ? Math.max(30+depth*10, origin-90) : 30+depth*10;
        const box = note && options.showDetails ? make("rect", {x:labelX, y:y-15, width:width-labelX-24, height:24, class:"sequence-note"}) : null;
        if (box) group.append(box);
        const extra = sourceControl(group, step, labelX, y); y += extra;
        if (box) box.setAttribute("height", 24+extra);
        if (step.kind === "call" && positions.has(step.target)) { y += 16; drawArrow(group, step); y += 24; }
        else y += 28;
      }
    }
  }
  draw(view.steps, svg, 0);
  y = Math.max(y+16, 160);
  for (const x of positions.values()) lines.append(make("line", {x1:x, x2:x, y1:54, y2:y-10}));
  svg.setAttribute("viewBox", `0 0 ${width} ${y}`); svg.setAttribute("height", y);
  container.append(svg);
  return groupControls;
}

window.BaleygSequence = {render: renderSequence};
