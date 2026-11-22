// The show (administrator only): a read-only board, two clocks and the drive. No server loop moves the bots: this view's
// own reads do, so it keeps polling even in a hidden tab, says so when it falters, and keeps the screen awake.
(() => {
  const { gt, gtp } = window.GT;
  const { api, h, formatClock, stonesOf, createBoard, confirmDialog } = window.Gomoku;

  const BOT_POLL_MS = 750;
  const IDLE_POLL_MS = 2000;
  const SILENCE_MS = 5000;
  const FAILURES_BEFORE_STALL = 3;
  const READ_TIMEOUT_MS = 8000;
  const TICK_MS = 1000;
  const STORAGE_KEY = "gomoku.show.projector";
  const MASKED_CODE = "••••-••••";

  const root = document.getElementById("show-room");
  const initial = JSON.parse(document.getElementById("show-initial").textContent);
  const gameId = root.dataset.gameId;

  let st = initial.state;
  let answerAt = performance.now();
  let lastOkAt = performance.now();
  let failures = 0;
  let hidden = document.visibilityState === "hidden";
  let reading = false;
  let again = false;
  let timer = null;
  let gone = false;
  let revealed = false;
  let copyNote = null;
  let announcement = "";
  let ending = false;
  let logoutNote = null;
  let wakeLock = null;
  const announced = new Set();

  const params = new URLSearchParams(window.location.search);
  const stored = (() => { try { return window.localStorage.getItem(STORAGE_KEY); } catch { return null; } })();
  let projector = params.get("projektor") === "1" || (params.get("projektor") === null && stored === "1");
  if (projector && params.get("projektor") === null) {
    params.set("projektor", "1");
    window.history.replaceState(null, "", `${window.location.pathname}?${params}`);
  }

  const board = createBoard({ onCell: () => {} });
  const colourOf = (seat) => (seat === st.blackSeat ? "black" : "white");
  const megabytes = (mb) => gt("common.bot.megabytes", { mb });
  const open = () => st.status === "active" || st.status === "waiting";

  const seatName = (player) => {
    if (!player) return "";
    if (player.kind === "bot" && player.bot) {
      return player.bot.tableMb !== null && player.bot.tableMb !== undefined
        ? gt("show.bot.nameWithTable", { level: player.bot.level, table: megabytes(player.bot.tableMb) })
        : gt("show.bot.name", { level: player.bot.level });
    }
    if (player.kind === "agent") return player.agentName ? gt("show.agent.declared", { name: player.agentName }) : gt("show.agent.name");
    return player.displayName ?? "";
  };
  const nameOf = (seat) => seatName(st.players.find((p) => p.seat === seat));

  // ---- the drive ---------------------------------------------------------------------------------------------------
  const merge = (current, next, after) => {
    if (next.moveCount < current.moves.length) return current;
    if (current.status === "finished" && next.status !== "finished") return current;
    return { ...next, moves: current.moves.slice(0, after).concat(next.moves) };
  };

  const schedule = () => {
    window.clearTimeout(timer);
    if (!open() || gone || reading) return;
    timer = window.setTimeout(readOnce, st.drive?.botOnTurn ? BOT_POLL_MS : IDLE_POLL_MS);
  };

  const readOnce = async () => {
    if (gone) return;
    if (reading) { again = true; return; }
    reading = true;
    const after = st.moves.length;
    const controller = new AbortController();
    const abort = window.setTimeout(() => controller.abort(), READ_TIMEOUT_MS);
    const result = await api(`/api/show/${gameId}?after=${after}`, { method: "GET", signal: controller.signal });
    window.clearTimeout(abort);
    reading = false;
    if (result.ok) {
      failures = 0; lastOkAt = performance.now();
      st = merge(st, result.data, after);
      answerAt = performance.now();
    } else if (result.error.code === "NOT_FOUND") gone = true;
    else failures += 1;
    render();
    if (again) { again = false; readOnce(); } else schedule();
  };

  const driveState = () => {
    if (hidden) return { state: "stalled", reason: "hidden" };
    if (st.drive?.stalled) return { state: "stalled", reason: "bot" };
    const silent = failures >= FAILURES_BEFORE_STALL || performance.now() - lastOkAt > SILENCE_MS + IDLE_POLL_MS;
    if (silent) return { state: "stalled", reason: "silent" };
    if (failures >= 1) return { state: "slowed" };
    return { state: "running" };
  };

  const countdown = (ms) => {
    const total = Math.max(0, Math.ceil(ms / 1000));
    return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
  };

  const requestWake = async () => {
    try {
      if (open() && !hidden && "wakeLock" in navigator && !wakeLock) {
        wakeLock = await navigator.wakeLock.request("screen");
        wakeLock.addEventListener("release", () => { wakeLock = null; });
      }
    } catch { wakeLock = null; }
  };

  document.addEventListener("visibilitychange", () => {
    hidden = document.visibilityState === "hidden";
    if (!hidden) { requestWake(); readOnce(); }
    render();
  });

  // ---- pieces ------------------------------------------------------------------------------------------------------
  const alertBox = (kind, ...children) => h("section", { class: `alert ${kind}`, role: kind === "danger" ? "alert" : "status" }, ...children);

  const clockEl = (seat) => {
    const running = st.clock.running === seat;
    return h("div", { class: `clock${running ? " running" : ""}`, "data-clock-seat": seat, "data-testid": `show-clock-${colourOf(seat)}` },
      h("span", { class: "clock-label" }, gt(`show.room.side.${colourOf(seat)}`, { name: nameOf(seat) })),
      running ? [" ", h("span", { class: "badge" }, gt("show.room.clockActive"))] : null,
      " ", h("span", { class: "clock-time" }, ""));
  };

  const driveIndicator = () => {
    const drive = driveState();
    const attrs = { "data-testid": "show-drive", "data-state": drive.state };
    if (drive.reason) attrs["data-reason"] = drive.reason;
    if (drive.state === "running") return h("p", { ...attrs, class: "quiet" }, gt("show.room.drive.running"));
    if (drive.state === "slowed") return h("p", { ...attrs, class: "alert warning", role: "status" }, gt("show.room.drive.slowed"));
    const seconds = Math.max(1, Math.round((performance.now() - lastOkAt) / 1000));
    const reasonText = drive.reason === "silent" ? gtp("show.room.drive.reason.silent", seconds) : gt(`show.room.drive.reason.${drive.reason}`);
    const onTurn = st.turnSeat ? st.clock.remainingMs[st.turnSeat - 1] - (performance.now() - answerAt) : null;
    if (!announced.has(drive.reason)) announced.add(drive.reason);
    return h("section", { ...attrs, class: "alert danger" },
      h("span", { class: "sr-only", role: "alert", "data-testid": "show-drive-announcement" }, gt("show.room.drive.stalledBecause", { reason: gt(`show.room.drive.announce.${drive.reason}`) })),
      h("strong", null, gt("show.room.drive.stalledBecause", { reason: reasonText })), " ", gt(`show.room.drive.hint.${drive.reason}`),
      onTurn !== null ? [" ", gt("show.room.drive.clock", { time: countdown(onTurn) })] : null, " ",
      h("button", { type: "button", "data-testid": "show-drive-check", onclick: readOnce }, gt("show.room.drive.check")));
  };

  const copyCode = async (code) => {
    try { await navigator.clipboard.writeText(code); copyNote = gt("show.room.agent.copied"); }
    catch { copyNote = gt("show.room.agent.copyFailed"); }
    render();
  };

  const agentCard = () => {
    const inv = st.invitation;
    const left = Date.parse(inv.expiresAt) - (st.clock.now + (performance.now() - answerAt));
    return h("section", { class: "card", "data-testid": "show-agent-card" },
      h("h2", null, gt("show.room.agent.title")),
      h("p", { "data-testid": "show-agent-waiting" }, gt("show.room.agent.waiting", { time: countdown(left) })),
      h("p", null, gt("show.room.agent.body")),
      h("p", null, gt("show.room.agent.codeLabel"), ": ",
        h("code", { "data-testid": "show-agent-code", "aria-hidden": revealed ? null : "true" }, revealed ? inv.code : MASKED_CODE),
        revealed ? null : h("span", { class: "sr-only", "data-testid": "show-agent-code-hidden" }, gt("show.room.agent.codeHidden")), " ",
        h("button", { type: "button", "data-testid": "show-agent-reveal", "aria-pressed": String(revealed), onclick: () => { revealed = !revealed; render(); } }, gt(revealed ? "show.room.agent.hide" : "show.room.agent.show")), " ",
        h("button", { type: "button", "data-testid": "show-agent-copy", onclick: () => copyCode(inv.code) }, gt("show.room.agent.copy"))),
      h("p", { role: "status", "data-testid": "show-agent-copy-status" }, copyNote ?? ""),
      h("p", null, gt("show.room.agent.mcpLabel"), ": ", h("code", { "data-testid": "show-agent-mcp" }, inv.mcpUrl ?? "")));
  };

  const endShow = () => confirmDialog({
    title: gt("show.end.title"), text: gt("show.end.body"), confirm: gt("show.end.confirm"), cancel: gt("show.end.cancel"), testid: "end-dialog",
    onConfirm: async () => {
      if (ending) return true;
      ending = true;
      const result = await api(`/api/show/${gameId}/end`);
      ending = false;
      if (result.ok) { st = merge(st, result.data, st.moves.length); render(); return true; }
      if (result.error.code === "GAME_NOT_ACTIVE") { readOnce(); return true; }
      return gt("show.end.failed");
    },
  });

  const logout = async () => {
    const result = await api("/api/auth/logout");
    if (result.ok) window.location.assign("/"); else { logoutNote = gt("show.room.logoutFailed"); render(); }
  };

  const setProjector = (on) => {
    projector = on;
    const next = new URLSearchParams(window.location.search);
    next.set("projektor", on ? "1" : "0");
    window.history.replaceState(null, "", `${window.location.pathname}?${next}`);
    try { window.localStorage.setItem(STORAGE_KEY, on ? "1" : "0"); } catch { /* the stored value is only a default */ }
    announcement = gt(on ? "show.room.projectorOn" : "show.room.projectorOff");
    render();
  };

  const statusBox = () => {
    if (st.status === "cancelled") return alertBox("warning", gt(st.closedReason === "expired" ? "show.room.expired" : "show.room.ended"));
    if (st.status === "finished" && st.result) {
      const winnerColour = st.result.winnerSeat ? colourOf(st.result.winnerSeat) : null;
      const title = winnerColour ? gt(`show.room.wins.${winnerColour}`, { name: nameOf(st.result.winnerSeat) }) : gt("show.room.draw");
      return h("section", { class: "result neutral", role: "status", "data-testid": "result" }, h("strong", null, title), " ", gt(`show.room.reason.${st.result.reason}`));
    }
    if (st.status === "active" && st.turnSeat) {
      return h("p", { class: "turn", role: "status", "data-testid": "turn-status" }, gt(`show.room.turn.${colourOf(st.turnSeat)}`, { name: nameOf(st.turnSeat) }));
    }
    return null;
  };

  const render = () => {
    if (gone) {
      root.replaceChildren(h("h1", null, gt("show.room.goneTitle")), h("p", null, gt("show.room.goneBody")), h("p", null, h("a", { href: "/show" }, gt("show.room.goneAction"))));
      return;
    }
    const stones = stonesOf(st.moves);
    board.update({ stones, last: st.moves.length ? st.moves[st.moves.length - 1] : null, winLine: st.result?.winLine ?? [], readOnly: true });
    board.el.setAttribute("aria-label", gt("show.board.label"));
    const stage = h("div", { class: "show-stage", "data-testid": "show-stage" });
    if (projector) stage.setAttribute("data-projector", "");
    const black = st.blackSeat ?? 1;
    const white = black === 1 ? 2 : 1;
    const side = h("div", { class: "show-side" },
      h("h1", null, open() ? gt("show.room.title", { layout: gt(`show.layout.${st.layout}`) }) : gt("show.room.finishedTitle")),
      h("label", null, h("input", { type: "checkbox", "data-testid": "show-projector", checked: projector, onchange: (e) => setProjector(e.target.checked) }), " ", gt("show.room.projector")),
      h("p", { class: "sr-only", role: "status", "aria-live": "polite", "data-testid": "show-announcement" }, announcement),
      open() ? driveIndicator() : null,
      statusBox(),
      st.status === "waiting" && st.invitation ? agentCard() : null,
      st.blackSeat ? [clockEl(black), clockEl(white)] : null,
      open() ? h("p", null, h("button", { type: "button", class: "danger", "data-testid": "show-end", onclick: endShow }, gt("show.end.action")))
        : h("p", null, h("a", { class: "button", href: "/show" }, gt("show.room.newShow")), " ", h("button", { type: "button", onclick: logout }, gt("show.room.logout")),
          logoutNote ? h("span", { role: "alert" }, ` ${logoutNote}`) : null));
    stage.append(h("div", { class: "show-board" }, board.el), side);
    root.replaceChildren(stage);
    tick();
  };

  const tick = () => {
    const elapsed = performance.now() - answerAt;
    for (const el of root.querySelectorAll("[data-clock-seat]")) {
      const seat = Number(el.dataset.clockSeat);
      const running = st.clock.running === seat;
      const left = Math.max(0, st.clock.remainingMs[seat - 1] - (running ? elapsed : 0));
      el.querySelector(".clock-time").textContent = formatClock(left);
      if (running && left <= 0 && open()) readOnce();
    }
    const waiting = root.querySelector("[data-testid=show-agent-waiting]");
    if (waiting && st.invitation) {
      const left = Date.parse(st.invitation.expiresAt) - (st.clock.now + elapsed);
      waiting.textContent = gt("show.room.agent.waiting", { time: countdown(left) });
      if (left <= 0) readOnce();
    }
  };

  window.setInterval(() => { if (!gone) { tick(); if (open() && !reading && (performance.now() - lastOkAt > SILENCE_MS) && document.visibilityState === "visible") render(); } }, TICK_MS);
  render();
  requestWake();
  schedule();
})();
