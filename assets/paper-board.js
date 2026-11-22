// The sheet of the paper and pencil variant: a window onto a 101x101 board without borders. The view logic is pure and
// the DOM follows the contract (grid of buttons with absolute-coordinate labels, fives drawn as lines, zoom and pan).
(() => {
  const { gt } = window.GT;
  const { h } = window.Gomoku;

  const SIZE = 101;
  const CENTER = 50;
  const PHONE_LEVELS = [15, 17, 19, 21];
  const DESKTOP_LEVELS = [15, 17, 19, 21, 23, 25, 27, 29, 31];
  const DRAG_PX = 8;
  const SVG_NS = "http://www.w3.org/2000/svg";

  // ---- pure view logic --------------------------------------------------------------------------------------------
  const half = (level) => Math.floor(level / 2);
  const clampView = (v) => {
    const k = half(v.level);
    return { level: v.level, cx: Math.min(SIZE - 1 - k, Math.max(k, v.cx)), cy: Math.min(SIZE - 1 - k, Math.max(k, v.cy)) };
  };
  const makeView = (level, cx, cy) => clampView({ level, cx, cy });
  const inWindow = (v, x, y) => Math.abs(x - v.cx) <= half(v.level) && Math.abs(y - v.cy) <= half(v.level);
  const revealPoint = (v, p) => {
    const k = half(v.level);
    return clampView({ ...v, cx: Math.min(p.x + k, Math.max(p.x - k, v.cx)), cy: Math.min(p.y + k, Math.max(p.y - k, v.cy)) });
  };
  const stonesCenter = (stones) => {
    if (!stones.length) return { x: CENTER, y: CENTER };
    const xs = stones.map((s) => s.x);
    const ys = stones.map((s) => s.y);
    return { x: Math.round((Math.min(...xs) + Math.max(...xs)) / 2), y: Math.round((Math.min(...ys) + Math.max(...ys)) / 2) };
  };
  const zoomView = (v, step, levels) => {
    const index = levels.indexOf(v.level);
    const next = levels[Math.min(levels.length - 1, Math.max(0, index + step))];
    return clampView({ ...v, level: next });
  };
  const arrowOf = (v, p) => {
    const k = half(v.level);
    const d = (delta) => (Math.abs(delta) <= k ? 0 : Math.sign(delta));
    const dx = d(p.x - v.cx);
    const dy = d(p.y - v.cy);
    return dx === 0 && dy === 0 ? null : { dx, dy };
  };
  const ARROWS = { "-1,-1": "↖", "0,-1": "↑", "1,-1": "↗", "-1,0": "←", "1,0": "→", "-1,1": "↙", "0,1": "↓", "1,1": "↘" };

  // ---- marks ------------------------------------------------------------------------------------------------------
  const svg = (tag, attrs, ...children) => {
    const el = document.createElementNS(SVG_NS, tag);
    for (const [k, v] of Object.entries(attrs ?? {})) el.setAttribute(k, String(v));
    el.append(...children);
    return el;
  };

  /** A cross (black) or a nought (white); `auto` marks the automat's stones with a dashed stroke. */
  const paperMark = (colour, { auto = false, framed = false, preview = false } = {}) => {
    const stroke = { stroke: auto || preview ? "var(--paper-auto, currentColor)" : "var(--paper-ink, currentColor)", "stroke-width": 3, "stroke-linecap": "round", fill: "none" };
    if (auto) stroke["stroke-dasharray"] = "4 2.5";
    const shape = colour === "black"
      ? svg("path", { d: "M5.5 5.5 18.5 18.5 M18.5 5.5 5.5 18.5", ...stroke })
      : svg("circle", { cx: 12, cy: 12, r: 7.5, ...stroke });
    const mark = svg("svg", { viewBox: "0 0 24 24", "aria-hidden": "true", class: framed ? "paper-mark framed" : "paper-mark" }, shape);
    return mark;
  };

  // ---- the component ------------------------------------------------------------------------------------------------
  const createPaperBoard = ({ onSelect, phone }) => {
    const levels = phone ? PHONE_LEVELS : DESKTOP_LEVELS;
    let view = makeView(phone ? 15 : 21, CENTER, CENTER);
    let props = { stones: [], fives: [], disabled: true, allowed: () => true, colors: { 1: "black", 2: "white" }, last: null, opponent: null, selected: null, token: "", pendingStone: null };
    let lastToken = null;
    let focus = null;
    let drag = null;
    let suppress = false;

    const root = h("div", { class: "paper-board", "data-testid": "paper-board" });
    const sheet = h("div", { class: "paper-sheet", "data-testid": "paper-sheet" });
    const grid = h("div", { role: "grid", "aria-label": gt("game.paper.board"), class: "paper-grid", "data-testid": "paper-grid" });
    const frame = h("div", { class: "paper-frame" }, grid);
    sheet.append(frame);

    const stoneMap = () => {
      const map = new Map();
      for (const s of props.stones) map.set(`${s.x},${s.y}`, s);
      if (props.pendingStone) map.set(`${props.pendingStone.x},${props.pendingStone.y}`, { ...props.pendingStone, pending: true });
      return map;
    };

    const setView = (next, rerender = true) => { view = clampView(next); if (rerender) render(); };

    const labelOf = (x, y, stone, refused, last, selected) => {
      const parts = [gt("game.paper.cell", { x: x - CENTER, y: y - CENTER })];
      parts.push(stone ? gt(`game.paper.state.${props.colors[stone.seat]}`) : gt("game.paper.state.empty"));
      if (!stone && refused) parts.push(gt("game.paper.tooFar"));
      if (last) parts.push(gt("game.paper.last"));
      if (stone?.auto) parts.push(gt("game.paper.auto"));
      if (selected) parts.push(gt("game.paper.selected"));
      return parts.join(", ");
    };

    const render = () => {
      const k = half(view.level);
      const x0 = view.cx - k;
      const y0 = view.cy - k;
      const stones = stoneMap();
      const last = props.last;
      const rows = [];
      const tabTarget = focus && inWindow(view, focus.x, focus.y) ? focus : { x: view.cx, y: view.cy };
      for (let r = 0; r < view.level; r += 1) {
        const y = y0 + r;
        const cells = [];
        for (let c = 0; c < view.level; c += 1) {
          const x = x0 + c;
          const stone = stones.get(`${x},${y}`);
          const refused = !stone && !props.allowed(x, y);
          const isLast = last && last.x === x && last.y === y;
          const isSel = props.selected && props.selected.x === x && props.selected.y === y;
          const button = h("button", {
            type: "button",
            class: `paper-cell${stone ? ` stone ${props.colors[stone.seat]}` : ""}${refused ? " refused" : ""}${isLast ? " last" : ""}${isSel ? " selected" : ""}${x === 0 ? " edge-left" : ""}${y === 0 ? " edge-top" : ""}`,
            "data-x": x, "data-y": y,
            "aria-label": labelOf(x, y, stone, refused, isLast, isSel),
            "aria-disabled": stone || refused || props.disabled ? "true" : null,
            tabindex: x === tabTarget.x && y === tabTarget.y ? "0" : "-1",
            onclick: () => {
              if (suppress) { suppress = false; return; }
              if (props.disabled || stone || refused) return;
              onSelect({ x, y });
            },
            onfocus: () => { focus = { x, y }; },
            onkeydown: (e) => {
              const d = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] }[e.key];
              if (!d) return;
              e.preventDefault();
              const p = { x: Math.min(100, Math.max(0, x + d[0])), y: Math.min(100, Math.max(0, y + d[1])) };
              focus = p;
              setView(revealPoint(view, p));
              grid.querySelector(`[data-x="${p.x}"][data-y="${p.y}"]`)?.focus();
            },
          }, stone ? paperMark(props.colors[stone.seat], { auto: stone.auto, preview: stone.pending }) : null);
          cells.push(h("div", { role: "gridcell" }, button));
        }
        rows.push(h("div", { role: "row", class: "paper-row" }, cells));
      }
      grid.style.setProperty("--level", String(view.level));
      grid.replaceChildren(...rows);
      const lines = svg("svg", { class: "paper-fives", viewBox: `0 0 ${view.level} ${view.level}`, preserveAspectRatio: "none", "aria-hidden": "true" },
        ...props.fives.map((f) => {
          const dx = Math.sign(f.to.x - f.from.x);
          const dy = Math.sign(f.to.y - f.from.y);
          const ux = dx * 0.3;
          const uy = dy * 0.3;
          return svg("line", { "data-seat": f.seat, x1: f.from.x - x0 + 0.5 - ux, y1: f.from.y - y0 + 0.5 - uy, x2: f.to.x - x0 + 0.5 + ux, y2: f.to.y - y0 + 0.5 + uy, stroke: "var(--paper-pencil, currentColor)", "stroke-width": 0.14, "stroke-linecap": "round" });
        }));
      frame.querySelector(".paper-fives")?.remove();
      frame.append(lines);
      renderChrome();
    };

    const tools = h("div", { class: "paper-tools" });
    const renderChrome = () => {
      const btn = (label, action, extra = {}) => h("button", { type: "button", "aria-label": label, onclick: action, ...extra }, extra.text ?? label);
      const step = Math.floor(view.level / 3);
      const pan = (dx, dy) => () => setView({ ...view, cx: view.cx + dx * step, cy: view.cy + dy * step });
      const zoomOutDisabled = view.level === levels[levels.length - 1];
      const zoomInDisabled = view.level === levels[0];
      const zoomButtons = [
        h("button", { type: "button", onclick: () => setView(zoomView(view, 1, levels)), disabled: zoomOutDisabled || null }, gt("game.paper.zoomOut")),
        h("button", { type: "button", onclick: () => setView(zoomView(view, -1, levels)), disabled: zoomInDisabled || null }, gt("game.paper.zoomIn")),
      ];
      const arrow = props.opponent ? arrowOf(view, props.opponent) : null;
      const marker = arrow
        ? h("button", { type: "button", class: "paper-opponent", "data-testid": "paper-opponent-marker", onclick: () => setView({ ...view, cx: props.opponent.x, cy: props.opponent.y }) },
          gt("game.paper.opponentMarker", { arrow: ARROWS[`${arrow.dx},${arrow.dy}`] }))
        : null;
      const parts = phone
        ? [h("div", { role: "group", "aria-label": gt("game.paper.selectionBar"), class: "paper-selection-bar" },
            ...[["Left", -1, 0], ["Up", 0, -1], ["Down", 0, 1], ["Right", 1, 0]].map(([name, dx, dy]) =>
              h("button", { type: "button", disabled: props.disabled || null, onclick: () => moveSelection(dx, dy) }, gt(`game.paper.selection${name}`))),
            ...zoomButtons)]
        : [h("div", { role: "toolbar", "aria-label": gt("game.paper.toolbar"), class: "paper-toolbar" },
            btn(gt("game.paper.panLeft"), pan(-1, 0), { text: "←" }), btn(gt("game.paper.panUp"), pan(0, -1), { text: "↑" }),
            btn(gt("game.paper.panDown"), pan(0, 1), { text: "↓" }), btn(gt("game.paper.panRight"), pan(1, 0), { text: "→" }),
            h("button", { type: "button", onclick: () => center() }, gt("game.paper.center")), ...zoomButtons,
            h("span", { "aria-live": "polite" }, gt("game.paper.level", { level: view.level })))];
      tools.replaceChildren(...[...parts, marker].filter(Boolean));
    };

    const center = () => {
      const p = props.last ?? stonesCenter(props.stones);
      setView({ ...view, cx: p.x, cy: p.y });
    };

    const moveSelection = (dx, dy) => {
      const from = props.selected ?? props.last ?? { x: CENTER, y: CENTER };
      const p = { x: Math.min(100, Math.max(0, from.x + dx)), y: Math.min(100, Math.max(0, from.y + dy)) };
      onSelect(p, { move: true });
      setView(revealPoint(view, p));
    };

    // Dragging pans the view opposite to the pointer; it suppresses the click that follows.
    frame.addEventListener("pointerdown", (e) => { drag = { x: e.clientX, y: e.clientY, cx: view.cx, cy: view.cy, moved: false }; });
    frame.addEventListener("pointermove", (e) => {
      if (!drag) return;
      const dx = e.clientX - drag.x;
      const dy = e.clientY - drag.y;
      if (!drag.moved && Math.hypot(dx, dy) < DRAG_PX) return;
      drag.moved = true;
      const cell = frame.clientWidth / view.level || 1;
      setView({ ...view, cx: drag.cx - Math.round(dx / cell), cy: drag.cy - Math.round(dy / cell) });
    });
    const endDrag = () => { if (drag?.moved) suppress = true; drag = null; };
    frame.addEventListener("pointerup", endDrag);
    frame.addEventListener("pointercancel", endDrag);
    frame.addEventListener("wheel", (e) => {
      if (!e.ctrlKey) return;
      e.preventDefault();
      setView(zoomView(view, e.deltaY < 0 ? -1 : 1, levels));
    }, { passive: false });

    const live = h("p", { class: "sr-only", role: "status", "data-testid": "paper-live" }, "");
    root.append(tools, sheet, live);
    // Order: tools above the sheet on desktop, the selection bar under it on the phone.
    if (phone) root.append(tools);

    return {
      el: root,
      update(next) {
        props = { ...props, ...next };
        if (props.token !== lastToken) {
          lastToken = props.token;
          const p = props.last ?? stonesCenter(props.stones);
          view = makeView(view.level, p.x, p.y);
        } else if (props.selected) view = revealPoint(view, props.selected);
        const lastStone = props.stones[props.stones.length - 1];
        live.textContent = lastStone?.auto ? gt("game.paper.autoMoved", { x: lastStone.x - CENTER, y: lastStone.y - CENTER }) : "";
        render();
      },
      view: () => view,
    };
  };

  // The pure parts are exposed for the checks that pin the contract.
  window.Gomoku.createPaperBoard = createPaperBoard;
  window.Gomoku.paperMark = paperMark;
  window.Gomoku.paperView = { makeView, clampView, revealPoint, stonesCenter, zoomView, arrowOf, inWindow, PHONE_LEVELS, DESKTOP_LEVELS, CENTER };
})();
