// Shared pieces of the game screens: API calls, a tiny DOM builder, the board, clock and result texts.
(() => {
  const { gt } = window.GT;
  const SIZE = 15;
  const COLUMNS = "ABCDEFGHIJKLMNO";

  /** Calls the JSON API; always resolves to the envelope (a failed fetch is `{ ok: false, error: { code: "network" } }`). */
  const api = async (path, { method = "POST", body, signal } = {}) => {
    try {
      const response = await fetch(path, {
        method,
        headers: { "X-Gomoku-Request": "1", ...(body === undefined ? {} : { "Content-Type": "application/json" }) },
        body: body === undefined ? undefined : JSON.stringify(body),
        credentials: "same-origin",
        signal,
      });
      const result = await response.json().catch(() => null);
      if (!result || typeof result.ok !== "boolean") return { ok: false, error: { code: "network" } };
      if (!result.ok) {
        const retry = Number(response.headers.get("Retry-After"));
        if (retry > 0) result.error.retryAfterSeconds = retry;
      }
      return result;
    } catch {
      return { ok: false, error: { code: "network" } };
    }
  };

  /** `h("p", { class: "x", "data-testid": "y", onclick: fn }, "text", child)`. Text is never parsed as HTML. */
  const h = (tag, attrs, ...children) => {
    const el = document.createElement(tag);
    for (const [name, value] of Object.entries(attrs ?? {})) {
      if (value === false || value === null || value === undefined) continue;
      if (name.startsWith("on")) el.addEventListener(name.slice(2), value);
      else el.setAttribute(name, value === true ? "" : String(value));
    }
    for (const child of children.flat()) {
      if (child === null || child === undefined || child === false) continue;
      el.append(child instanceof Node ? child : document.createTextNode(String(child)));
    }
    return el;
  };

  /** `m:ss`, rounding the seconds UP (a clock with 0.2 s left still shows 0:01). */
  const formatClock = (ms) => {
    const total = Math.max(0, Math.ceil(ms / 1000));
    return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
  };

  const cellParts = (cell) => ({ column: COLUMNS[cell % SIZE], row: Math.floor(cell / SIZE) + 1 });

  const cellName = (cell) => gt("game.board.cellName", { column: COLUMNS[cell % SIZE], row: Math.floor(cell / SIZE) + 1 });

  /** Stones by cell from the move log: black plays the even indices. */
  const stonesOf = (moves, upTo = moves.length) => {
    const stones = new Map();
    for (let i = 0; i < Math.min(upTo, moves.length); i += 1) stones.set(moves[i], i % 2 === 0 ? "black" : "white");
    return stones;
  };

  /**
   * The board: 15x15 intersections, one Tab stop (roving tabindex), arrows move, Enter/Space picks. `onCell(x, y)` fires on
   * activation; `update({ stones, last, winLine, readOnly, selected })` redraws only what changed.
   */
  const createBoard = ({ onCell }) => {
    const cells = [];
    let focused = 112;
    const el = h("div", { class: "board", role: "grid", "aria-label": gt("game.shared.boardLabel"), "data-testid": "board" });
    for (let cell = 0; cell < SIZE * SIZE; cell += 1) {
      const x = cell % SIZE;
      const y = Math.floor(cell / SIZE);
      const button = h("button", {
        type: "button", class: "cell", role: "gridcell", tabindex: cell === focused ? "0" : "-1",
        "data-testid": `cell-${y}-${x}`, "data-cell": cell,
      });
      button.append(h("span", { class: "stone", "aria-hidden": "true" }));
      cells.push(button);
      el.append(button);
    }
    const label = (cell, stone) => {
      const state = gt(`game.board.state.${stone ?? "empty"}`);
      return gt("game.board.cell", { column: COLUMNS[cell % SIZE], row: Math.floor(cell / SIZE) + 1, state });
    };
    const focus = (cell) => {
      cells[focused].tabIndex = -1;
      focused = cell;
      cells[cell].tabIndex = 0;
      cells[cell].focus();
    };
    el.addEventListener("click", (event) => {
      const button = event.target.closest(".cell");
      if (!button) return;
      const cell = Number(button.dataset.cell);
      focus(cell);
      onCell(cell % SIZE, Math.floor(cell / SIZE));
    });
    el.addEventListener("keydown", (event) => {
      const delta = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] }[event.key];
      if (!delta) return;
      event.preventDefault();
      const x = Math.min(SIZE - 1, Math.max(0, (focused % SIZE) + delta[0]));
      const y = Math.min(SIZE - 1, Math.max(0, Math.floor(focused / SIZE) + delta[1]));
      focus(y * SIZE + x);
    });
    const update = ({ stones, last, winLine = [], readOnly, selected = null, selectedColor = "black" }) => {
      el.setAttribute("aria-readonly", readOnly ? "true" : "false");
      cells.forEach((button, cell) => {
        const stone = stones.get(cell) ?? (cell === selected ? selectedColor : null);
        const preview = !stones.has(cell) && cell === selected;
        const stoneEl = button.firstChild;
        stoneEl.className = stone ? `stone ${stone}${preview ? " preview" : ""}` : "stone";
        button.classList.toggle("last", cell === last);
        button.classList.toggle("win", winLine.includes(cell));
        button.setAttribute("aria-label", label(cell, stones.get(cell)));
        button.disabled = false;
        button.dataset.readonly = readOnly ? "true" : "false";
      });
    };
    return { el, update };
  };

  /** The result of a finished game from the viewpoint of `mySeat`: the banner kind and its two texts. */
  const scoreText = (result, mySeat) => `${result.scores[mySeat - 1]} : ${result.scores[2 - mySeat]}`;

  const outcomeOf = (result, mySeat, paper = false) => {
    if (paper || result.scores) {
      const score = result.scores ? scoreText(result, mySeat) : "";
      if (result.reason === "points") {
        if (result.winnerSeat === null) return { kind: "draw", title: score ? gt("game.outcome.drawScore", { score }) : gt("game.outcome.draw"), detail: score ? gt("game.outcome.pointsDetail") : "" };
        const won = result.winnerSeat === mySeat;
        return { kind: won ? "win" : "loss", title: score ? gt(won ? "game.outcome.winScore" : "game.outcome.lossScore", { score }) : gt(won ? "game.outcome.pointsWon" : "game.outcome.pointsLost"), detail: score ? gt("game.outcome.pointsDetail") : "" };
      }
      if (result.reason === "resignation") {
        const won = result.winnerSeat === mySeat;
        return score
          ? { kind: won ? "win" : "loss", title: gt(won ? "game.outcome.resignWonScore" : "game.outcome.resignLostScore"), detail: gt("game.outcome.resignedScore", { score }) }
          : { kind: won ? "win" : "loss", title: gt(won ? "game.outcome.resignWon" : "game.outcome.resignLost"), detail: "" };
      }
    }
    if (result.reason === "draw") return { kind: "draw", title: gt("game.outcome.draw"), detail: gt("game.outcome.drawFull") };
    const won = result.winnerSeat === mySeat;
    const titles = {
      five_in_row: [gt("game.outcome.won"), gt("game.outcome.lost")],
      timeout: [gt("game.outcome.timeoutWon"), gt("game.outcome.timeoutLost")],
      resignation: [gt("game.outcome.resignWon"), gt("game.outcome.resignLost")],
    }[result.reason] ?? [gt("game.outcome.won"), gt("game.outcome.lost")];
    const detail = result.reason === "five_in_row" ? gt(won ? "game.outcome.fiveWon" : "game.outcome.fiveLost") : "";
    return { kind: won ? "win" : "loss", title: titles[won ? 0 : 1], detail };
  };

  /** The banner for a reader who did not play (a spectator, a review by code): never "you won". */
  const neutralOutcomeOf = (result, blackSeat, players, paper = false) => {
    const set = paper ? "paper" : "classic";
    const score = result.scores ? gt("game.outcome.neutral.score", { score: `${Math.max(...result.scores)} : ${Math.min(...result.scores)}` }) : "";
    if (result.reason === "draw") return { kind: "draw", title: gt("game.outcome.draw"), detail: gt("game.outcome.neutral.draw") };
    if (result.winnerSeat === null) return { kind: "draw", title: gt("game.outcome.neutral.drawScore", { score: result.scores ? `${result.scores[0]} : ${result.scores[1]}` : "" }), detail: gt("game.outcome.neutral.pointsEqual") };
    const winner = players.find((p) => p.seat === result.winnerSeat);
    const colour = result.winnerSeat === blackSeat ? "black" : "white";
    return {
      kind: "neutral",
      title: gt("game.outcome.neutral.win", { wins: gt(`game.sides.${set}.wins.${colour}`), name: winner ? playerLabelOf(winner) : "", score }),
      detail: gt(`game.outcome.neutral.${result.reason}`),
    };
  };

  /** "Bot · poziom 2 · 16 MB" built in the viewer's language from the strength the server sends. */
  const botLabel = (player) => {
    const bot = player.bot;
    if (!bot) return player.displayName ?? gt("common.bot.plain");
    if (bot.level === 2 && bot.tableMb !== null && bot.tableMb !== undefined) {
      return gt("common.bot.nameWithTable", { level: bot.level, table: gt("common.bot.megabytes", { mb: bot.tableMb }) });
    }
    return gt("common.bot.name", { level: bot.level });
  };

  /** "Agent" or "Agent · name (declared)": the name is the agent's own claim, shown as plain text and cut when long. */
  const agentLabel = (player) => {
    const name = typeof player.agentName === "string" ? player.agentName.trim() : "";
    return name ? gt("common.player.agentDeclared", { name }) : gt("common.player.agent");
  };

  const playerLabelOf = (player) => playerLabel(player, null);
  const playerLabel = (player, mySeat) => {
    const mine = player.seat === mySeat;
    if (player.kind === "bot") return botLabel(player);
    if (player.kind === "agent") return agentLabel(player);
    if (player.kind === "account") {
      if (player.displayName === null) return gt("common.player.deleted");
      return mine ? gt("common.player.youAccount", { name: player.displayName }) : player.displayName;
    }
    const match = /^Gracz ([12])$/.exec(player.displayName ?? "");
    const name = match ? gt("common.player.guestDefault", { seat: match[1] }) : player.displayName;
    const label = gt("common.player.guestLabel", { name });
    return mine ? `${gt("common.player.youGuestLead")}${label}` : label;
  };

  /** Dates in a fixed zone so the server and every browser agree. */
  const formatDate = (iso) =>
    new Intl.DateTimeFormat(window.GT.lang, { dateStyle: "long", timeStyle: "short", timeZone: "Europe/Warsaw" }).format(new Date(iso));

  window.Gomoku = { api, h, formatClock, cellName, cellParts, stonesOf, createBoard, outcomeOf, neutralOutcomeOf, playerLabel, formatDate, SIZE };
})();
