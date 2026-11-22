// The game room: renders every state of a game from the JSON the server serves and keeps it fresh by polling.
// Constants are those of the reference implementation (see .ai/docs/2026-11-22-f3-game-contract.md, section 7).
(() => {
  const { gt, gtp } = window.GT;
  const { api, h, formatClock, stonesOf, createBoard, outcomeOf, playerLabel, formatDate, cellName, cellParts, paperMark, createPaperBoard } = window.Gomoku;

  const POLL_MS = 3000;
  const PLAY_POLL_MS = 2000;
  const BOT_POLL_MS = 750;
  const IDLE_POLL_MS = 12000;
  const IDLE_AFTER_MS = 180000;
  const POLL_TIMEOUT_MS = 8000;
  const JOIN_STEP_MS = 2500;
  const RETURN_NOTICE_MS = 8000;
  const LONG_ABSENCE_MS = 30000;
  const RETURN_FRESH_MS = 10000;
  const GUEST_RESULT_WINDOW_MS = 55 * 60 * 1000;
  const LEAVE_DECLINE_TIMEOUT_MS = 3000;
  const LOW_MS = 60000;
  const POLL_FAIL_LIMIT = 3;

  const root = document.getElementById("game-room");
  const initial = JSON.parse(document.getElementById("game-initial").textContent);
  const gameId = root.dataset.gameId;
  const coarse = window.matchMedia?.("(pointer: coarse)").matches ?? false;

  let game = initial.view;
  let answerAt = performance.now();
  let gone = false;
  let fails = 0;
  let hidden = document.visibilityState === "hidden";
  let reading = false;
  let readAgain = false;
  let timer = null;
  let joinStepUntil = 0;
  let version = 0;
  let quietSince = performance.now();
  let hiddenAt = null;
  let returnedAt = null;
  let returnNotice = null;
  let expiredFor = null;
  let pendingCell = null;
  let selected = null;
  let moveBusy = false;
  let refusal = null;
  let moveFailed = null;
  let lastMoveAttempt = null;
  let rematchBusy = null;
  let rematchMsg = null;
  let rematchWithdrawn = false;
  let rematchHidden = false;
  let accepting = false;
  let lastSignature = "";
  let readAbort = null;
  let ownWaitingNotice = initial.notice === "own-waiting";
  let tokenNoticeDismissed = false;
  const wasFinishedOnLoad = game.status === "finished";

  const me = () => game.you.seat;
  const other = () => game.players.find((p) => p.seat !== me());
  const mySide = () => (game.blackSeat === me() ? "black" : "white");
  const isGuest = game.you.kind === "guest";
  const serverNow = () => game.clock.now + (performance.now() - answerAt);
  const allSeen = () => game.players.length === 2 && game.players.every((p) => p.seen);
  const myTurn = () => game.status === "active" && game.turnSeat === me();

  const boardCtl = createBoard({ onCell: (x, y) => onCell(x, y) });

  // ---- the paper variant ----------------------------------------------------------------------------------------------
  const isPaper = () => game.variant === "paper";
  const botOf = () => game.players.find((p) => p.kind === "bot") ?? null;
  const botThinking = () => game.status === "active" && game.turnSeat !== null && botOf()?.seat === game.turnSeat;
  const known = () => (isPaper() ? game.paper.moves.length : game.moves.length);
  const lowMs = () => (isPaper() ? Math.min(60000, game.timeControlMs / 4) : 60000);
  let pendingXY = null;
  let paperSelected = null;
  let recenters = 0;
  const paperBoard = createPaperBoard({ phone: coarse, onSelect: (p, { move = false } = {}) => onPaperSelect(p, move) });
  const colourOfSeat = (seat) => (seat === game.blackSeat ? "black" : "white");
  const paperStones = () => game.paper.moves.map((m) => ({ x: m.x, y: m.y, seat: m.seat, auto: m.auto }));
  const autoSeat = () => game.paper?.autoSeat ?? null;
  const paperMyTurn = () => game.status === "active" && game.turnSeat === me() && autoSeat() !== game.turnSeat;
  const refusalOf = (x, y) => {
    const stones = paperStones();
    if (stones.some((s) => s.x === x && s.y === y)) return "CELL_OCCUPIED";
    if (stones.length === 0) return x === 50 && y === 50 ? null : "FIRST_MOVE_CENTER";
    return stones.some((s) => Math.abs(s.x - x) <= 3 && Math.abs(s.y - y) <= 3) ? null : "TOO_FAR";
  };

  // Figures are built once, so the browser loads each QR image once and not on every render.
  const qrFigure = (alt, caption) => h("figure", { class: "qr-figure" },
    h("img", { src: `/api/games/${gameId}/qr`, alt, class: "qr" }), h("figcaption", null, caption));
  const qrInvite = qrFigure(gt("game.room.qrLabel"), gt("game.room.qrCaption"));
  const qrPublic = h("section", { class: "card", "data-testid": "qr-panel" }, h("h2", null, gt("game.audience.qrTitle")),
    qrFigure(gt("game.audience.qrLabel"), gt("game.audience.qrCaption")));
  const chat = window.GomokuChat.createChat({
    base: `/api/games/${gameId}`,
    mySeat: me(),
    players: () => game.players,
    onGone: () => { gone = true; render(); },
  });
  let chatStarted = false;
  const flag = () => (game.audience
    ? h("p", { "data-testid": "public-flag", class: "badge" }, gt("game.audience.publicFlag"), h("span", { "data-testid": "public-flag-count" }, String(game.audience.spectators)))
    : null);
  const openCloseAudience = () => confirmDialog({
    title: gt("game.audience.closeTitle"), text: gt("game.audience.closeBody"), confirm: gt("game.audience.close"),
    cancel: gt("game.audience.keep"), testid: "close-audience-dialog",
    onConfirm: async () => {
      const result = await api(`/api/games/${gameId}/audience/close`);
      if (result.ok) { apply(result.data, 0); return true; }
      const code = result.error.code;
      if (code === "NOT_FOUND") { gone = true; render(); return true; }
      if (["GAME_NOT_ACTIVE", "GAME_NOT_PUBLIC"].includes(code)) { readOnce(); return gt("game.audience.notClosable"); }
      return gt(code === "network" ? "game.shared.network" : "game.audience.closeFailed");
    },
  });
  const audienceBits = () => [
    flag(),
    game.canCloseAudience ? h("p", null, h("button", { type: "button", "data-testid": "close-audience", onclick: openCloseAudience }, gt("game.audience.close"))) : null,
    game.audienceClosed ? h("p", { role: "status", "data-testid": "audience-closed-note" }, gt("game.audience.closedNote")) : null,
  ];

  // ---- state merging and application --------------------------------------------------------------------------------
  const merge = (current, next, after) => {
    const count = current.variant === "paper" ? current.paper.moves.length : current.moves.length;
    if (next.moveCount < count) return current; // an older answer
    if (current.status === "finished" && next.status !== "finished") return current;
    if (next.variant === "paper") {
      return { ...next, paper: { ...next.paper, moves: current.paper.moves.slice(0, after).concat(next.paper.moves) } };
    }
    return { ...next, moves: current.moves.slice(0, after).concat(next.moves) };
  };

  /** Local change of the state (e.g. the rematch the player just offered): no merge, no new answer time. */
  const patchGame = (partial) => {
    game = { ...game, ...partial };
    render();
    schedule();
  };

  const apply = (next, after, { fromRead = false, startedAt = null } = {}) => {
    const before = game;
    game = merge(game, next, after);
    if (game === before) return;
    answerAt = performance.now();
    version += 1;
    if (before.status === "waiting" && game.status === "active" && game.you.seat === 1) joinStepUntil = performance.now() + JOIN_STEP_MS;
    if (before.moveCount !== game.moveCount) { selected = null; refusal = null; moveFailed = null; }
    if (returnNotice && returnNotice.moveCount !== game.moveCount) returnNotice = null;
    if (fromRead && returnedAt !== null && startedAt !== null && startedAt >= returnedAt
        && performance.now() - returnedAt <= RETURN_FRESH_MS && game.status === "active" && allSeen()) {
      returnNotice = { moveCount: game.moveCount, text: gt("game.room.returned", { clock: formatClock(game.clock.remainingMs[me() - 1]) }) };
      window.setTimeout(() => { returnNotice = null; render(); }, RETURN_NOTICE_MS);
      returnedAt = null;
    }
    if (game.rematch && before.rematch?.state !== game.rematch.state) quietSince = performance.now();
    expiredFor = null;
    render();
    schedule();
    maybeFollowRematch();
  };

  // ---- reading --------------------------------------------------------------------------------------------------------
  const readOnce = async () => {
    if (reading || gone || accepting) { if (reading) readAgain = true; return; }
    reading = true;
    const seq = version;
    const startedAt = performance.now();
    const after = known();
    readAbort = new AbortController();
    const abortTimer = window.setTimeout(() => readAbort.abort(), POLL_TIMEOUT_MS);
    const result = await api(`/api/games/${gameId}?after=${after}`, { method: "GET", signal: readAbort.signal });
    window.clearTimeout(abortTimer);
    reading = false;
    if (accepting) { readAgain = false; return; }
    if (result.ok) {
      fails = 0;
      if (seq === version || result.data.moveCount >= known()) apply(result.data, after, { fromRead: true, startedAt });
      else render();
    } else if (result.error.code === "NOT_FOUND") {
      gone = true;
      render();
    } else {
      fails += 1;
      render();
    }
    if (readAgain) { readAgain = false; readOnce(); } else schedule();
  };

  const needsPolling = () => {
    if (gone || hidden || accepting) return false;
    if (game.status === "waiting" || game.status === "active") return true;
    if (game.status === "finished") {
      if (game.rematch?.state === "accepted" || botOf()) return false;
      if (isGuest && game.finishedAt && serverNow() >= Date.parse(game.finishedAt) + GUEST_RESULT_WINDOW_MS) return false;
      return true;
    }
    return false;
  };

  const delay = () => {
    if (botThinking()) return BOT_POLL_MS;
    if (game.status === "active" && allSeen()) return PLAY_POLL_MS;
    if (game.status === "finished" && performance.now() - quietSince > IDLE_AFTER_MS) return IDLE_POLL_MS;
    return POLL_MS;
  };

  const schedule = () => {
    window.clearTimeout(timer);
    timer = null;
    if (!needsPolling() || reading) return;
    timer = window.setTimeout(readOnce, delay());
  };

  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") {
      hidden = true;
      hiddenAt = performance.now();
      window.clearTimeout(timer);
      return;
    }
    hidden = false;
    quietSince = performance.now();
    if (hiddenAt !== null && performance.now() - hiddenAt > LONG_ABSENCE_MS) returnedAt = performance.now();
    hiddenAt = null;
    if (reading) readAgain = true; else readOnce();
  });
  window.addEventListener("focus", () => { quietSince = performance.now(); if (!hidden && !reading) readOnce(); });

  // ---- moves ----------------------------------------------------------------------------------------------------------
  const sendMove = async (x, y, moveNumber) => {
    if (moveBusy) return;
    moveBusy = true;
    pendingCell = isPaper() ? y * 101 + x : y * 15 + x;
    pendingXY = { x, y };
    lastMoveAttempt = { x, y, moveNumber };
    refusal = null;
    moveFailed = null;
    render();
    const result = await api(`/api/games/${gameId}/moves`, { body: { x, y, moveNumber } });
    moveBusy = false;
    pendingCell = null;
    pendingXY = null;
    if (result.ok) {
      lastMoveAttempt = null;
      apply(result.data, moveNumber - 1);
      return;
    }
    const code = result.error.code;
    if (code === "NOT_FOUND") { gone = true; render(); return; }
    if (["STALE_MOVE", "NOT_YOUR_TURN", "CELL_OCCUPIED", "GAME_FINISHED", "GAME_NOT_ACTIVE", "MOVE_TOO_FAR", "FIRST_MOVE_CENTER"].includes(code)) {
      refusal = code === "GAME_NOT_ACTIVE" ? "GAME_FINISHED" : code;
      lastMoveAttempt = null;
      render();
      readOnce();
      return;
    }
    moveFailed = { network: code === "network" };
    render();
  };

  const onPaperSelect = (p, move) => {
    if (!paperMyTurn() || moveBusy || !allSeen()) return;
    if (move) { paperSelected = p; render(true); return; }
    if (refusalOf(p.x, p.y)) return;
    if (coarse) { paperSelected = p; render(true); return; }
    sendMove(p.x, p.y, game.moveCount + 1);
  };

  const onCell = (x, y) => {
    if (game.status !== "active" || !myTurn() || moveBusy || !allSeen()) return;
    const cell = y * 15 + x;
    if (stonesOf(game.moves).has(cell)) return;
    if (coarse) { selected = cell; render(); return; }
    sendMove(x, y, game.moveCount + 1);
  };

  // ---- small view helpers ---------------------------------------------------------------------------------------------
  const alertBox = (kind, ...children) => h("section", { class: `alert ${kind}`, role: kind === "danger" ? "alert" : "status" }, ...children);
  const homeLink = (key = "game.shared.backHome") => h("a", { href: "/" }, gt(key));
  const clockEl = (seat, label) =>
    h("div", { class: "clock", "data-clock-seat": seat, "data-testid": `clock-${seat === me() ? "me" : "opponent"}` },
      h("span", { class: "clock-label" }, label), " ", h("span", { class: "clock-time" }, ""));

  const createGame = async (button) => {
    button.disabled = true;
    const result = await api("/api/games", { body: {} });
    if (result.ok) window.location.assign(`/games/${result.data.gameId}`);
    else { button.disabled = false; window.location.assign("/"); }
  };

  const confirmDialog = ({ title, text, confirm, cancel, onConfirm, testid }) => {
    const dialog = h("dialog", { class: "dialog", "data-testid": testid });
    const error = h("p", { role: "alert" }, "");
    const back = h("button", { type: "button", autofocus: true, onclick: () => dialog.close() }, cancel);
    const yes = h("button", { type: "button", class: "danger", "data-testid": `${testid}-confirm`, onclick: async () => {
      yes.disabled = true;
      const outcome = await onConfirm();
      if (outcome === true) dialog.close(); else { yes.disabled = false; error.textContent = outcome || ""; }
    } }, confirm);
    dialog.append(h("h2", null, title), h("p", null, text), error, h("div", { class: "actions" }, back, yes));
    document.body.append(dialog);
    dialog.addEventListener("close", () => dialog.remove());
    dialog.showModal();
    back.focus();
  };

  // ---- screens --------------------------------------------------------------------------------------------------------
  const screenGone = () => [
    h("h1", { "data-testid": "gate-title" }, gt("game.gate.title")), h("p", null, gt("game.gate.body")), h("p", null, homeLink()),
  ];

  const screenCancelled = () => {
    const reason = game.closedReason;
    if (reason === "not_opened") {
      const mine = game.players.find((p) => p.seat === me());
      return [
        h("h1", null, gt("game.shared.cancelledHeading")),
        alertBox("warning", h("strong", null, gt(mine?.seen ? "game.room.notOpenedByOpponent" : "game.room.notOpened")), " ", gt("game.room.nobodyLost")),
        h("p", null, homeLink()),
      ];
    }
    const heading = gt(reason === "expired" ? "game.room.expiredHeading" : "game.room.cancelledHeading");
    const details = gt(reason === "expired" ? "game.room.expiredDetails" : "game.room.cancelledDetails");
    const again = isGuest
      ? h("a", { class: "button", href: "/play" }, gt("game.room.createNew"))
      : h("button", { type: "button", "data-testid": "create-game", onclick: (e) => createGame(e.currentTarget) }, gt("game.room.createNew"));
    return [
      h("h1", null, heading),
      alertBox("warning", h("strong", null, gt("game.room.createNewTitle", { heading })), " ", details),
      h("p", null, again), h("p", null, homeLink()),
    ];
  };

  const screenWaiting = () => {
    const inv = game.invitation;
    const status = fails >= POLL_FAIL_LIMIT
      ? alertBox("danger", gt("game.room.joinCheckFailed"), " ", h("button", { type: "button", onclick: () => readOnce() }, gt("game.shared.checkNow")))
      : h("p", { "data-testid": "waiting-status", role: "status" }, fails >= 1 ? gt("game.room.waitingRetry", { attempt: fails + 1, max: POLL_FAIL_LIMIT }) : gt("game.room.waiting"));
    const copyStatus = h("p", { role: "status", "data-testid": "copy-status" }, "");
    const linkInput = h("input", { type: "text", readonly: true, value: inv?.url ?? "", "data-testid": "invite-link", "aria-label": gt("game.room.linkLabel") });
    const copy = h("button", { type: "button", "data-testid": "copy", onclick: async () => {
      try { await navigator.clipboard.writeText(inv.url); copyStatus.textContent = gt("game.room.copy.ok"); }
      catch { linkInput.select(); copyStatus.textContent = gt("game.room.copy.fail"); }
    } }, gt("game.room.copyLabel"));
    const share = navigator.share ? h("button", { type: "button", onclick: async () => {
      try { await navigator.share({ title: gt("game.room.shareTitle"), url: inv.url }); }
      catch (error) { if (error?.name !== "AbortError") copyStatus.textContent = gt("game.room.copy.share-fail"); }
    } }, gt("game.room.share")) : null;
    const cancel = h("button", { type: "button", "data-testid": "cancel-invitation", onclick: () => confirmDialog({
      title: gt("game.room.cancelTitle"), text: gt("game.room.cancelDescription"), confirm: gt("game.room.cancelConfirm"),
      cancel: gt("game.shared.back"), testid: "cancel-dialog",
      onConfirm: async () => {
        const result = await api(`/api/games/${gameId}/cancel`);
        if (result.ok) { window.location.replace("/?notice=cancelled"); return true; }
        if (result.error.code === "GAME_NOT_CANCELLABLE") { cancelNote = "joined-first"; readOnce(); return true; }
        if (result.error.code === "NOT_FOUND") { gone = true; render(); return true; }
        return gt(result.error.code === "network" ? "game.room.cancelFailedNetwork" : "game.room.cancelFailedBody");
      },
    }) }, gt("game.room.cancelInvitation"));
    return [
      h("h1", null, gt("game.room.waitingHeading")),
      ownWaitingNotice ? alertBox("info", gt("game.room.ownWaiting"), " ", h("button", { type: "button", onclick: () => { ownWaitingNotice = false; render(); } }, gt("game.room.close"))) : null,
      cancelNote === "joined-first" ? alertBox("warning", h("strong", null, gt("game.room.joinedFirstTitle")), " ", gt("game.room.joinedFirstBody")) : null,
      status,
      inv ? h("section", { class: "card", "data-testid": "invite-card" },
        h("h2", null, gt("game.room.inviteTitle")),
        h("label", null, gt("game.room.linkLabel"), linkInput),
        h("p", null, gt("game.room.codeLabel"), ": ", h("strong", { "data-testid": "invite-code" }, inv.code)),
        h("p", null, gt("game.room.validUntil", { date: formatDate(inv.expiresAt) })),
        qrInvite,
        h("p", null, copy, " ", share, " ", cancel), copyStatus) : null,
      isGuest ? alertBox("warning", gt("game.room.keepBrowser")) : null,
      h("section", { "data-testid": "chat-panel" }, h("h2", null, gt("chat.title")), h("p", null, gt("chat.waiting"))),
      h("p", null, homeLink()),
    ];
  };
  let cancelNote = null;

  const screenJoined = () => [
    h("h1", null, gt("game.room.joinedHeading")),
    alertBox("success", h("strong", null, gt("game.room.joinedTitle", { name: playerLabel(other(), me()) })), " ", gt("game.room.joinedBody")),
    h("button", { type: "button", "data-testid": "go-to-game", onclick: () => { joinStepUntil = 0; render(); } }, gt("game.room.goToGame")),
  ];

  const screenOpening = () => [
    h("h1", null, gt("game.room.openingHeading")),
    h("p", { role: "status", "data-testid": "opening-status" }, gt("game.room.opening", { name: playerLabel(other(), me()) })),
    clockEl(other().seat, playerLabel(other(), me())), clockEl(me(), playerLabel(game.players.find((p) => p.seat === me()), me())),
    isGuest ? alertBox("warning", h("strong", null, gt("game.shared.guestWarningTitle")), " ", gt("game.shared.guestWarningBody")) : null,
    h("p", null, homeLink()),
  ];

  const boardHost = h("div", { class: "board-host" });
  const rematchHost = h("div", { class: "rematch-host" });

  /** A player's name in the paper variant: the mark, the label, the points and a note when the automat plays for them. */
  const paperName = (player) => h("span", null,
    paperMark(colourOfSeat(player.seat), { framed: true }), playerLabel(player, me()),
    h("span", { "data-testid": player.seat === me() ? "score-mine" : "score-theirs" }, gt("game.shared.score", { score: game.paper.scores[player.seat - 1] })),
    autoSeat() === player.seat ? gt("game.shared.automat") : "");

  const screenBoard = () => {
    const finished = game.status === "finished";
    const my = game.players.find((p) => p.seat === me());
    const opp = other();
    const side = mySide();
    const paper = isPaper();
    const myT = paper ? paperMyTurn() : myTurn();
    if (paper) {
      const stones = paperStones();
      const lastStone = stones.length ? stones[stones.length - 1] : null;
      const mine = stones.filter((st) => st.seat === me()).length;
      const opponentLast = [...stones].reverse().find((st) => st.seat !== me()) ?? null;
      if (paperSelected === null && myT && stones.length === 0) paperSelected = { x: 50, y: 50 };
      if (!myT || (paperSelected && stones.some((st) => st.x === paperSelected.x && st.y === paperSelected.y))) paperSelected = myT ? paperSelected : null;
      paperBoard.update({
        stones, fives: game.paper.fives, last: lastStone, opponent: opponentLast, pendingStone: pendingXY ? { ...pendingXY, seat: me(), auto: false } : null,
        disabled: finished || !myT || moveBusy, allowed: (x, y) => refusalOf(x, y) === null,
        colors: { 1: colourOfSeat(1), 2: colourOfSeat(2) }, selected: coarse ? paperSelected : null,
        token: `${mine}:${recenters}`,
      });
      boardHost.replaceChildren(paperBoard.el);
    } else {
      const stones = stonesOf(game.moves);
      const last = game.moves.length ? game.moves[game.moves.length - 1] : null;
      const display = new Map(stones);
      if (pendingCell !== null && !display.has(pendingCell)) display.set(pendingCell, side);
      const readOnly = finished || !myTurn() || moveBusy;
      boardCtl.update({ stones: display, last, winLine: game.result?.winLine ?? [], readOnly, selected, selectedColor: side });
      boardHost.replaceChildren(boardCtl.el);
    }
    const sideSet = paper ? "paper" : "classic";

    let banner;
    if (finished && game.result) {
      const outcome = outcomeOf(game.result, me(), paper);
      banner = h("section", { class: `result ${outcome.kind}`, role: "status", "data-testid": "result" }, h("strong", null, outcome.title), outcome.detail ? [" ", outcome.detail] : null);
    } else {
      banner = h("p", { class: "turn", role: "status", "data-testid": "turn-status" },
        moveBusy ? gt("game.field.sending")
          : myT ? gt("game.field.yourTurn", { side: gt(`game.sides.${sideSet}.name.${side}`) })
          : paper && autoSeat() === me() && game.turnSeat !== me() ? gt("game.field.automatForYou")
          : paper && autoSeat() === game.turnSeat ? gt("game.field.automatMoves")
          : botThinking() ? gt("game.field.botThinking")
          : gt("game.field.opponentTurn"));
    }
    const polling = !finished && fails >= 1
      ? (fails >= POLL_FAIL_LIMIT
        ? alertBox("danger", gt("game.shared.pollFailed"), " ", h("button", { type: "button", onclick: () => readOnce() }, gt("game.shared.checkNow")))
        : h("p", { role: "status" }, gt("game.shared.pollRetry", { attempt: fails + 1, max: POLL_FAIL_LIMIT })))
      : null;
    const paperRefusal = paper && paperSelected ? refusalOf(paperSelected.x, paperSelected.y) : null;
    const selectedText = paper
      ? (paperSelected === null
        ? (myT ? gt("game.field.paperHint") : gt(autoSeat() !== null ? "game.field.waitAutomat" : "game.field.waitOpponent"))
        : paperRefusal ? gt(`game.paper.refusal.${paperRefusal}`)
        : gt("game.field.paperSelected", { x: paperSelected.x - 50, y: paperSelected.y - 50, place: gt(`game.sides.paper.place.${side}`) }))
      : (selected === null
        ? (myTurn() ? gt("game.field.classicHint") : gt("game.field.waitOpponent"))
        : gt("game.field.classicSelected", { cell: cellName(selected), place: gt(`game.sides.classic.place.${side}`) }));
    const placeDisabled = paper
      ? paperSelected === null || moveBusy || !myT || Boolean(paperRefusal)
      : selected === null || moveBusy || !myTurn();
    const place = () => {
      if (paper) { if (paperSelected) sendMove(paperSelected.x, paperSelected.y, game.moveCount + 1); }
      else if (selected !== null) sendMove(selected % 15, Math.floor(selected / 15), game.moveCount + 1);
    };
    const touch = coarse && !finished
      ? h("div", { class: "touch" },
        h("button", { type: "button", "data-testid": "place", disabled: placeDisabled, onclick: place }, gt(`game.sides.${sideSet}.place.${side}`)),
        paper ? h("button", { type: "button", "data-testid": "paper-recenter", onclick: () => { recenters += 1; render(true); } }, gt("game.field.recenter")) : null,
        h("p", { role: "status", "data-testid": "selection" }, selectedText))
      : null;
    const refusalBox = refusal ? alertBox("warning", gt(`game.field.refusals.${refusal}`)) : null;
    const failedBox = moveFailed ? alertBox("danger", gt(moveFailed.network ? "game.shared.network" : "game.field.moveFailed"), " ",
      h("button", { type: "button", "data-testid": "retry-move", onclick: () => lastMoveAttempt && sendMove(lastMoveAttempt.x, lastMoveAttempt.y, lastMoveAttempt.moveNumber) }, gt("game.shared.retry"))) : null;
    const plays = gt(`game.sides.${sideSet}.plays.${side}`);
    const drawn = game.moveCount === 0 && !finished
      ? alertBox("info", game.fromRematch ? gt("game.field.drawnRematch", { swapped: gt(`game.sides.${sideSet}.rematchSwapped`), plays }) : gt("game.field.drawn", { plays }))
      : null;
    const opponentMove = !paper && myTurn() && game.moves.length
      ? h("p", { class: "sr-only", role: "status", "data-testid": "opponent-move" }, gt("game.board.opponentMove", cellParts(game.moves[game.moves.length - 1])))
      : null;
    const resign = !finished ? h("button", { type: "button", class: "danger", "data-testid": "resign", onclick: openResign }, gt("game.field.resign")) : null;
    const guestNotes = [];
    if (isGuest && !finished) guestNotes.push(alertBox("warning", h("strong", null, gt("game.shared.guestWarningTitle")), " ", gt("game.shared.guestWarningBody")));
    if (isGuest && finished) guestNotes.push(h("p", { "data-testid": "guest-until" }, gt("game.field.guestUntil")));
    if (isGuest && game.fromRematch && !tokenNoticeDismissed) {
      guestNotes.push(alertBox("info", h("strong", null, gt("game.field.newAccessTitle")), " ", gt("game.field.newAccessBody"), " ",
        h("button", { type: "button", "data-testid": "token-notice-dismiss", onclick: () => { tokenNoticeDismissed = true; render(); } }, gt("game.field.understood"))));
    }
    return [
      h("h1", { "data-testid": "room-heading" }, gt(finished ? "game.shared.finishedHeading" : "game.field.inProgress")),
      ...audienceBits(),
      returnNotice ? alertBox("info", returnNotice.text) : null,
      polling, banner,
      finished && wasFinishedOnLoad ? h("p", { role: "status", "data-testid": "reload-note" }, gt("game.field.reloaded")) : null,
      h("p", { class: "sr-only", role: "status", "data-testid": "low-live" }, lowAnnounced ? gt(paper ? "game.field.paperLowTime" : "game.field.lowTime") : ""),
      clockEl(opp.seat, paper ? paperName(opp) : playerLabel(opp, me())), boardHost, opponentMove, touch, refusalBox, failedBox, ...guestNotes, drawn,
      clockEl(me(), paper ? paperName(my) : playerLabel(my, me())),
      h("p", { "data-testid": "my-color" }, gt(finished ? "game.field.youPlayed" : "game.field.youPlay", { plays })),
      resign, rematchHost,
      game.audience ? qrPublic : null,
      botOf() ? null : chat.el,
    ];
  };

  const openResign = () => confirmDialog({
    title: gt("game.field.resignTitle"), text: gt("game.field.resignBody"), confirm: gt("game.field.resign"),
    cancel: gt("game.field.backToGame"), testid: "resign-dialog",
    onConfirm: async () => {
      const result = await api(`/api/games/${gameId}/resign`);
      if (result.ok) { apply(result.data, 0); return true; }
      if (result.error.code === "NOT_FOUND") { gone = true; render(); return true; }
      if (["GAME_FINISHED", "GAME_NOT_ACTIVE"].includes(result.error.code)) { refusal = "GAME_FINISHED"; readOnce(); render(); return true; }
      return gt(result.error.code === "network" ? "game.shared.network" : "game.field.resignFailed");
    },
  });

  // ---- rematch --------------------------------------------------------------------------------------------------------
  const rematchMode = () => {
    const r = game.rematch;
    if (!r) return { kind: "none" };
    if (r.state === "accepted") return r.gameId ? { kind: "accepted", gameId: r.gameId } : { kind: "none" };
    if (r.state === "declined") return r.offeredBySeat === me() && !rematchWithdrawn ? { kind: "declined" } : { kind: "none" };
    if (r.state === "expired") return { kind: "expired" };
    if (Date.parse(r.expiresAt) <= serverNow()) return { kind: "expired" };
    return { kind: r.offeredBySeat === me() ? "mine" : "theirs" };
  };

  const rematchCall = async (action) => {
    if (rematchBusy) return null;
    rematchBusy = action;
    rematchMsg = null;
    renderRematch();
    const result = await api(`/api/games/${gameId}/rematch${action === "offer" ? "" : `/${action}`}`);
    rematchBusy = null;
    return result;
  };

  const offerRematch = async () => {
    rematchWithdrawn = false;
    const result = await rematchCall("offer");
    if (!result) return;
    if (result.ok) { quietSince = performance.now(); patchGame({ rematch: result.data }); return; }
    const code = result.error.code;
    if (code === "NOT_FOUND") { gone = true; render(); return; }
    if (["REMATCH_ALREADY_OFFERED", "GAME_NOT_FINISHED"].includes(code)) { rematchMsg = { kind: "info", text: gt("game.rematch.opponentFirst") }; readOnce(); }
    else if (code === "RATE_LIMITED") rematchMsg = { kind: "warning", text: gt("game.rematch.tooMany", { minutes: Math.max(1, Math.ceil((result.error.retryAfterSeconds ?? 60) / 60)) }) };
    else rematchMsg = { kind: "danger", text: gt(code === "network" ? "game.shared.network" : "game.rematch.failed"), retry: offerRematch };
    renderRematch();
  };

  const acceptRematch = async () => {
    const result = await rematchCall("accept");
    if (!result) return;
    if (result.ok) { accepting = true; window.clearTimeout(timer); readAbort?.abort(); window.location.assign(`/games/${result.data.gameId}`); return; }
    const error = result.error;
    if (error.code === "NOT_FOUND") { gone = true; render(); return; }
    if (error.code === "ACTIVE_GAME_EXISTS") rematchMsg = error.data?.gameId
      ? { kind: "warning", text: gt("game.rematch.ownActive"), link: [`/games/${error.data.gameId}`, gt("game.rematch.ownGame")] }
      : { kind: "warning", text: gt("game.rematch.opponentActive") };
    else if (error.code === "REMATCH_UNAVAILABLE") { rematchMsg = { kind: "warning", text: gt("game.rematch.unavailable") }; rematchHidden = true; }
    else if (error.code === "REMATCH_EXPIRED") { rematchMsg = { kind: "info", text: gt("game.rematch.expiredOffer") }; readOnce(); }
    else if (error.code === "REMATCH_NOT_OFFERED") { rematchMsg = { kind: "info", text: gt("game.rematch.withdrawnByOpponent") }; readOnce(); }
    else rematchMsg = { kind: "danger", text: gt(error.code === "network" ? "game.shared.network" : "game.rematch.failed") };
    renderRematch();
  };

  const declineRematch = async (withdraw) => {
    const result = await rematchCall("decline");
    if (!result) return;
    if (result.ok) { rematchWithdrawn = withdraw; patchGame({ rematch: { ...game.rematch, state: "declined" } }); rematchMsg = withdraw ? null : { kind: "info", text: gt("game.rematch.youDeclined") }; renderRematch(); return; }
    if (result.error.code === "NOT_FOUND") { gone = true; render(); return; }
    readOnce();
  };

  const leave = (event) => {
    const mode = rematchMode();
    if (!["mine", "theirs"].includes(mode.kind) || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    event.preventDefault();
    const controller = new AbortController();
    window.setTimeout(() => controller.abort(), LEAVE_DECLINE_TIMEOUT_MS);
    api(`/api/games/${gameId}/rematch/decline`, { signal: controller.signal }).finally(() => window.location.assign("/"));
  };

  // ---- the next game against the bot (there is no rematch) ----------------------------------------------------------
  let botReplayBusy = false;
  let botReplayMsg = null;
  const playBotAgain = async () => {
    if (botReplayBusy) return;
    botReplayBusy = true;
    botReplayMsg = null;
    render(true);
    const me0 = game.players.find((p) => p.seat === me());
    const body = { opponent: "bot", botLevel: botOf().bot?.level ?? 1 };
    if (isGuest) {
      body.acknowledgedGuestWarning = true;
      if (me0 && !/^Gracz [12]$/.test(me0.displayName ?? "")) body.nickname = me0.displayName;
    }
    const result = await api("/api/games", { body });
    if (result.ok) { window.location.assign(`/games/${result.data.gameId}`); return; }
    const error = result.error;
    botReplayBusy = false;
    if (error.code === "BOT_UNAVAILABLE") botReplayMsg = { kind: "warning", text: gt("game.botReplay.botUnavailable") };
    else if (error.code === "ACTIVE_GAME_EXISTS") botReplayMsg = { kind: "warning", title: gt("game.botReplay.activeTitle"), text: gt("game.botReplay.activeBody"), link: error.data?.gameId ? [`/games/${error.data.gameId}`, gt("game.botReplay.goToGame")] : null };
    else if (error.code === "RATE_LIMITED") botReplayMsg = { kind: "warning", title: gt("game.botReplay.limitTitle"), text: gtp("home.shared.createLimit", 10) };
    else botReplayMsg = { kind: "danger", title: gt("game.botReplay.failedTitle"), text: gt(error.code === "network" ? "game.botReplay.failedNetwork" : "game.botReplay.failedServer") };
    render(true);
  };
  const renderBotReplay = () => {
    const back = h("a", { href: "/", "data-testid": "rematch-leave" }, gt("game.shared.backToPanel"));
    rematchHost.replaceChildren(h("section", { "aria-labelledby": "bot-replay-h", "data-testid": "bot-replay" },
      h("h2", { id: "bot-replay-h" }, gt("game.botReplay.heading")),
      h("p", null, gt("game.botReplay.intro")),
      h("button", { type: "button", "data-testid": "bot-replay-again", disabled: botReplayBusy, onclick: playBotAgain }, gt(botReplayBusy ? "game.botReplay.creating" : "game.botReplay.playAgain")),
      botReplayMsg ? alertBox(botReplayMsg.kind, botReplayMsg.title ? h("strong", null, botReplayMsg.title) : null, botReplayMsg.title ? " " : null, botReplayMsg.text,
        botReplayMsg.link ? [" ", h("a", { href: botReplayMsg.link[0], "data-testid": "bot-replay-own-game" }, botReplayMsg.link[1])] : null) : null,
      back));
  };

  let countdownEl = null;
  const renderRematch = () => {
    if (game.status !== "finished") { rematchHost.replaceChildren(); return; }
    if (botOf()) { renderBotReplay(); return; }
    const mode = rematchMode();
    const opp = other();
    const gone2 = game.players.some((p) => p.kind === "account" && p.displayName === null);
    const plays = gt(`game.sides.classic.plays.${mySide() === "black" ? "white" : "black"}`);
    const parts = [h("h2", { id: "rematch-h" }, gt("game.rematch.heading"))];
    countdownEl = null;
    const back = h("a", { href: "/", "data-testid": "rematch-leave", onclick: leave }, gt("game.shared.backToPanel"));
    const button = (testid, label, busyKey, busyLabel, onclick) =>
      h("button", { type: "button", "data-testid": testid, disabled: Boolean(rematchBusy), onclick }, rematchBusy === busyKey ? busyLabel : label);
    const countdown = () => { countdownEl = h("span", { "data-testid": "rematch-countdown", "aria-hidden": "true" }, ""); return countdownEl; };
    if (gone2) {
      parts.push(alertBox("warning", h("strong", null, gt("game.rematch.unavailableTitle")), " ", gt("game.rematch.deletedAccount")), back);
    } else if (mode.kind === "accepted") {
      parts.push(h("p", { role: "status", "data-testid": "rematch-status" }, gt("game.rematch.accepted")));
    } else {
      if (mode.kind === "none") {
        parts.push(h("p", { "data-testid": "rematch-info" }, gt("game.rematch.info", { info: gt("game.sides.classic.rematchInfo"), plays })));
        if (isGuest) parts.push(h("p", { "data-testid": "guest-rematch-note" }, gt("game.rematch.guestNote")));
        parts.push(button("rematch-offer", gt("game.rematch.offer"), "offer", gt("game.rematch.sending"), offerRematch));
      } else if (mode.kind === "mine") {
        parts.push(h("p", { role: "status", "data-testid": "rematch-status" }, gt("game.rematch.offered"), " ", countdown()));
        parts.push(button("rematch-withdraw", gt("game.rematch.withdraw"), "decline", gt("game.rematch.withdrawing"), () => declineRematch(true)));
      } else if (mode.kind === "theirs") {
        parts.push(h("p", { role: "status", "data-testid": "rematch-status" }, gt("game.rematch.theirOffer", { name: playerLabel(opp, me()), swap: gt("game.sides.classic.rematchOffer") }), " ", countdown()));
        if (!rematchHidden) parts.push(button("rematch-accept", gt("game.rematch.accept"), "accept", gt("game.rematch.accepting"), acceptRematch));
        parts.push(button("rematch-decline", gt("game.rematch.decline"), "decline", gt("game.rematch.sending"), () => declineRematch(false)));
      } else if (mode.kind === "declined") {
        parts.push(h("p", { class: "sr-only", role: "status", "data-testid": "rematch-status" }, gt("game.rematch.declinedStatus")));
        parts.push(alertBox("warning", h("strong", null, gt("game.rematch.declinedTitle")), " ", gt("game.rematch.declinedBody")));
        parts.push(button("rematch-offer", gt("game.rematch.offerAgain"), "offer", gt("game.rematch.sending"), offerRematch));
      } else if (mode.kind === "expired") {
        parts.push(h("p", { role: "status", "data-testid": "rematch-status" }, gt("game.rematch.expired")));
        parts.push(button("rematch-offer", gt("game.rematch.offerAgain"), "offer", gt("game.rematch.sending"), offerRematch));
      }
      if (mode.kind === "none" && rematchMsg === null && !rematchWithdrawn && game.rematch?.state === "declined") parts.push(h("p", { role: "status" }, gt("game.rematch.youDeclined")));
      if (rematchMsg) {
        parts.push(alertBox(rematchMsg.kind, rematchMsg.text, rematchMsg.link ? [" ", h("a", { href: rematchMsg.link[0], "data-testid": "rematch-own-game" }, rematchMsg.link[1])] : null,
          rematchMsg.retry ? [" ", h("button", { type: "button", "data-testid": "rematch-retry", onclick: rematchMsg.retry }, gt("game.shared.retry"))] : null));
      }
      parts.push(back);
    }
    rematchHost.replaceChildren(h("section", { "aria-labelledby": "rematch-h", "data-testid": "rematch-box" }, ...parts));
    tickCountdown();
  };

  const tickCountdown = () => {
    if (!countdownEl || !game.rematch) return;
    const left = Math.max(0, Date.parse(game.rematch.expiresAt) - serverNow());
    const total = Math.ceil(left / 1000);
    countdownEl.textContent = gt("game.rematch.left", { time: `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}` });
  };

  // A guest first claims the token of the new game through the old one (a read sets the cookie), then follows.
  const maybeFollowRematch = async () => {
    const r = game.rematch;
    if (!r || r.state !== "accepted" || !r.gameId || accepting) return;
    accepting = true;
    window.clearTimeout(timer);
    if (isGuest) {
      const result = await api(`/api/games/${gameId}?after=${known()}`, { method: "GET" });
      if (!result.ok) {
        accepting = false;
        rematchMsg = { kind: "danger", text: gt("game.rematch.claimFailed"), retry: () => { rematchMsg = null; maybeFollowRematch(); } };
        if (result.error.code === "NOT_FOUND") { gone = true; }
        render();
        return;
      }
    }
    window.location.assign(`/games/${r.gameId}`);
  };

  // ---- clocks ---------------------------------------------------------------------------------------------------------
  let lowAnnounced = false;
  const paintClocks = () => {
    const elapsed = performance.now() - answerAt;
    const running = game.status === "finished" ? null : game.clock.running;
    let expiredNow = false;
    document.querySelectorAll("[data-clock-seat]").forEach((el) => {
      const seat = Number(el.dataset.clockSeat);
      const base = game.clock.remainingMs[seat - 1];
      const left = seat === running ? Math.max(0, base - elapsed) : base;
      el.querySelector(".clock-time").textContent = formatClock(left);
      el.dataset.low = left < lowMs() ? "true" : "false";
      el.classList.toggle("running", seat === running);
      if (seat === me() && left < lowMs() && !lowAnnounced && game.status === "active" && allSeen()) {
        lowAnnounced = true;
        const live = document.querySelector('[data-testid="low-live"]');
        if (live) live.textContent = gt(isPaper() ? "game.field.paperLowTime" : "game.field.lowTime");
      }
      if (seat === running && left <= 0) expiredNow = true;
    });
    // When the running clock hits zero locally the page reads at once; only the server decides the loss.
    if (expiredNow && expiredFor !== answerAt) { expiredFor = answerAt; readOnce(); }
    tickCountdown();
    if (game.rematch?.state === "pending" && rematchMode().kind === "expired") renderRematch();
  };

  const tickClocks = () => {
    paintClocks();
    window.setTimeout(tickClocks, 250);
  };

  // ---- rendering ------------------------------------------------------------------------------------------------------
  const screenOf = () => {
    if (gone) return "gone";
    if (game.status === "cancelled") return "cancelled";
    if (game.status === "waiting") return "waiting";
    if (performance.now() < joinStepUntil) return "joined";
    if (game.status === "active" && !allSeen()) return "opening";
    return "board";
  };

  const render = (force = false) => {
    const screen = screenOf();
    const focusCell = document.activeElement?.dataset?.cell ?? null;
    const stable = { ...game, clock: { remainingMs: game.clock.remainingMs, running: game.clock.running } };
    const signature = JSON.stringify([screen, stable, fails, moveBusy, pendingCell, selected, refusal, moveFailed, returnNotice?.text, ownWaitingNotice, cancelNote, tokenNoticeDismissed, rematchBusy, rematchMsg?.text, botReplayBusy, botReplayMsg?.text, rematchHidden, rematchWithdrawn, paperSelected, recenters, pendingXY]);
    if (signature === lastSignature && !force) return;
    lastSignature = signature;
    const builders = { gone: screenGone, cancelled: screenCancelled, waiting: screenWaiting, joined: screenJoined, opening: screenOpening, board: screenBoard };
    root.dataset.screen = screen;
    root.replaceChildren(...builders[screen]().filter(Boolean));
    if (screen === "board") {
      renderRematch();
      if (!chatStarted && !botOf()) { chatStarted = true; chat.start(); }
    }
    if (screen === "joined") window.setTimeout(render, JOIN_STEP_MS + 50);
    if (focusCell !== null) root.querySelector(`[data-cell="${focusCell}"]`)?.focus();
    paintClocks();
  };

  render();
  tickClocks();
  schedule();
  maybeFollowRematch();
})();
