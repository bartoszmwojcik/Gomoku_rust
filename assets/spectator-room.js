// The read-only room of a spectator of a public game: the board, the clocks, the audience flag, the QR code and the chat.
(() => {
  const { gt } = window.GT;
  const { api, h, formatClock, stonesOf, createBoard, neutralOutcomeOf, playerLabel } = window.Gomoku;

  const POLL_MS = 2000;
  const SLOW_POLL_MS = 5000;
  const SLOW_AFTER_MS = 60000;
  const TIMEOUT_MS = 8000;
  const FAILS_BEFORE_ALERT = 3;
  const LOW_MS = 60000;

  const root = document.getElementById("spectator-room");
  const initial = JSON.parse(document.getElementById("spectator-initial").textContent);
  const gameId = root.dataset.gameId;

  let game = initial.view;
  let answerAt = performance.now();
  let lastMoveAt = performance.now();
  let screen = "room";
  let fails = 0;
  let reading = false;
  let timer = null;
  let hidden = document.visibilityState === "hidden";
  let lastSig = "";

  const board = createBoard({ onCell: () => {} });
  const chat = window.GomokuChat.createChat({
    base: `/api/games/${gameId}/spectate`,
    spectator: true,
    mySeat: null,
    players: () => game.players,
    canWrite: () => game.canChat,
    // A 404 from the chat is told apart from a closed audience by re-reading the state.
    onGone: () => readOnce(),
    unavailableText: gt("chat.closed"),
  });

  const colourOf = (seat) => (seat === game.blackSeat ? "black" : "white");

  const merge = (current, next, after) => {
    if (next.moveCount < current.moves.length) return current;
    if (current.status === "finished" && next.status !== "finished") return current;
    return { ...next, moves: current.moves.slice(0, after).concat(next.moves) };
  };

  const needsPolling = () => screen === "room" && game.status === "active" && !hidden;
  const delay = () => (performance.now() - lastMoveAt > SLOW_AFTER_MS ? SLOW_POLL_MS : POLL_MS);
  const schedule = () => {
    window.clearTimeout(timer);
    if (needsPolling() && !reading) timer = window.setTimeout(readOnce, delay());
  };

  const readOnce = async () => {
    if (reading || screen !== "room") return;
    reading = true;
    const after = game.moves.length;
    const controller = new AbortController();
    const abort = window.setTimeout(() => controller.abort(), TIMEOUT_MS);
    const result = await api(`/api/games/${gameId}/spectate?after=${after}`, { method: "GET", signal: controller.signal });
    window.clearTimeout(abort);
    reading = false;
    if (result.ok) {
      fails = 0;
      const before = game;
      game = merge(game, result.data, after);
      if (game !== before) {
        answerAt = performance.now();
        if (before.moveCount !== game.moveCount) lastMoveAt = performance.now();
      }
    } else if (result.error.code === "NOT_FOUND") screen = "gone";
    else if (result.error.code === "AUDIENCE_CLOSED") { screen = "closed"; chat.stop(); }
    else fails += 1;
    render();
    schedule();
  };

  document.addEventListener("visibilitychange", () => {
    hidden = document.visibilityState === "hidden";
    if (hidden) window.clearTimeout(timer); else readOnce();
  });

  const alertBox = (kind, ...children) => h("section", { class: `alert ${kind}`, role: kind === "danger" ? "alert" : "status" }, ...children);
  const clockEl = (seat) => {
    const player = game.players.find((p) => p.seat === seat);
    return h("div", { class: "clock", "data-clock-seat": seat, "data-testid": `clock-${colourOf(seat)}` },
      h("span", { class: "clock-label" }, gt("game.spectator.sideName", { side: gt(`game.sides.classic.title.${colourOf(seat)}`) }), player ? playerLabel(player, null) : ""),
      " ", h("span", { class: "clock-time" }, ""));
  };

  const qrPanel = () => h("section", { class: "card", "data-testid": "qr-panel" },
    h("h2", null, gt("game.audience.qrTitle")),
    h("figure", null, h("img", { src: `/api/games/${gameId}/qr`, alt: gt("game.audience.qrLabel"), class: "qr" }),
      h("figcaption", null, gt("game.audience.qrCaption"))));

  const flag = () => (game.audience ? h("p", { "data-testid": "public-flag", class: "badge" }, gt("game.audience.publicFlag"), h("span", { "data-testid": "public-flag-count" }, String(game.audience.spectators))) : null);

  const roomScreen = () => {
    const finished = game.status === "finished";
    if (game.status === "cancelled") {
      return [h("h1", null, gt("game.shared.cancelledHeading")), alertBox("warning", gt("game.spectator.cancelled")), h("p", null, h("a", { href: "/" }, gt("game.shared.backHome")))];
    }
    const stones = stonesOf(game.moves);
    const last = game.moves.length ? game.moves[game.moves.length - 1] : null;
    board.update({ stones, last, winLine: game.result?.winLine ?? [], readOnly: true });
    let banner;
    if (finished && game.result) {
      const outcome = neutralOutcomeOf(game.result, game.blackSeat, game.players);
      banner = h("section", { class: `result ${outcome.kind}`, role: "status", "data-testid": "result" }, h("strong", null, outcome.title), " ", outcome.detail);
    } else if (game.turnSeat) {
      const on = game.players.find((p) => p.seat === game.turnSeat);
      banner = h("p", { class: "turn", role: "status", "data-testid": "turn-status" }, gt("game.spectator.turn", { side: gt(`game.sides.classic.genitive.${colourOf(game.turnSeat)}`), name: on ? playerLabel(on, null) : "" }));
    }
    const poll = !finished && fails >= 1
      ? (fails >= FAILS_BEFORE_ALERT
        ? alertBox("danger", gt("game.shared.pollFailed"), " ", h("button", { type: "button", onclick: () => readOnce() }, gt("game.shared.checkNow")))
        : h("p", { role: "status" }, gt("game.shared.pollRetry", { attempt: fails + 1, max: FAILS_BEFORE_ALERT })))
      : null;
    const black = game.blackSeat;
    const white = black === 1 ? 2 : 1;
    return [
      h("h1", null, gt(finished ? "game.shared.finishedHeading" : "game.spectator.watching")),
      alertBox("info", gt("game.spectator.youWatch")),
      flag(), poll, banner,
      clockEl(black), clockEl(white),
      h("div", { class: "board-host", "aria-label": gt("game.shared.boardLabel") }, board.el),
      game.audience ? qrPanel() : null,
      chat.el,
      h("p", null, h("a", { href: "/" }, gt("game.shared.backHome"))),
    ];
  };

  const render = () => {
    const sig = JSON.stringify([screen, { ...game, clock: { r: game.clock.remainingMs, u: game.clock.running } }, fails]);
    if (sig === lastSig) return;
    lastSig = sig;
    root.dataset.screen = screen;
    if (screen === "gone") {
      chat.stop();
      root.replaceChildren(h("h1", { "data-testid": "spectator-gone" }, gt("game.spectator.goneTitle")), h("p", null, gt("game.spectator.goneBody")), h("p", null, h("a", { href: "/join" }, gt("game.spectator.goneLink"))));
    } else if (screen === "closed") {
      root.replaceChildren(h("div", { "data-testid": "audience-closed" }, h("h1", null, gt("game.audience.closedTitle")), h("p", null, gt("game.audience.closedBody")), h("p", null, h("a", { href: "/" }, gt("game.shared.backHome")))));
    } else {
      const chatWasAttached = root.contains(chat.el);
      root.replaceChildren(...roomScreen().filter(Boolean));
      if (!chatWasAttached) chat.start();
      if (game.status === "cancelled") chat.stop();
    }
    paintClocks();
  };

  const paintClocks = () => {
    const elapsed = performance.now() - answerAt;
    const running = game.status === "finished" ? null : game.clock.running;
    let expired = false;
    document.querySelectorAll("[data-clock-seat]").forEach((el) => {
      const seat = Number(el.dataset.clockSeat);
      const base = game.clock.remainingMs[seat - 1];
      const left = seat === running ? Math.max(0, base - elapsed) : base;
      el.querySelector(".clock-time").textContent = formatClock(left);
      el.dataset.low = left < LOW_MS ? "true" : "false";
      el.classList.toggle("running", seat === running);
      if (seat === running && left <= 0) expired = true;
    });
    if (expired && !reading) readOnce();
  };

  render();
  window.setInterval(paintClocks, 250);
  schedule();
})();
