// The chat panel of a game, for a player (with the switch) and for a spectator (without). One polling loop of its own,
// independent of the room's state polling. Constants are those of the contract (3 s, idle 10 s after a quiet minute, 8 s).
(() => {
  const { gt, gtp } = window.GT;
  const { api, h, playerLabel } = window.Gomoku;

  const POLL_MS = 3000;
  const IDLE_POLL_MS = 10000;
  const IDLE_AFTER_MS = 60000;
  const TIMEOUT_MS = 8000;
  const PAGE = 50;
  const FAILS_BEFORE_ALERT = 3;
  const DEFAULT_WAIT_MS = 2000;

  const uuid = () => {
    if (window.crypto?.randomUUID) return window.crypto.randomUUID();
    const b = new Uint8Array(16);
    window.crypto.getRandomValues(b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    const x = [...b].map((n) => n.toString(16).padStart(2, "0")).join("");
    return `${x.slice(0, 8)}-${x.slice(8, 12)}-${x.slice(12, 16)}-${x.slice(16, 20)}-${x.slice(20)}`;
  };

  /** What the composer counts: code points of the NFKC form with ASCII spaces trimmed. */
  const lengthOf = (text) => [...text.normalize("NFKC").replace(/^ +| +$/g, "")].length;

  /**
   * options: base (API prefix without a trailing slash), spectator (bool), mySeat, players (() => list),
   * canWrite (() => bool, spectator only), onGone (called on a 404 so the room can tell why), unavailableText.
   */
  const createChat = (options) => {
    const { base, spectator = false } = options;
    const chatUrl = spectator ? `${base}/chat` : `${base}/chat`;
    let state = null;
    let messages = [];
    let cursor = 0;
    let limits = { maxLength: 200, minIntervalMs: 2000 };
    let retryUntil = 0;
    let opponentDisabled = false;
    let fails = 0;
    let folded = false;
    let foldedAt = 0;
    let infoOpen = false;
    let busy = false;
    let sending = null;
    let draft = "";
    let clientId = uuid();
    let lastText = "";
    let sendError = null;
    let switchBusy = false;
    let switchError = null;
    let epoch = 0;
    let reading = false;
    let timer = null;
    let lastNewAt = performance.now();
    let stopped = true;
    let stickBottom = true;
    let waitTimer = null;

    const root = h("section", { class: "chat", "data-testid": "chat-panel", "aria-labelledby": "chat-title" });
    const nameOf = (m) => {
      if (m.seat === null || m.seat === undefined) return h("span", { "data-testid": "chat-spectator", class: "chat-name" }, gt("chat.spectatorLabel", { name: m.spectatorName ?? "" }));
      const player = options.players().find((p) => p.seat === m.seat);
      return h("span", { "data-testid": "chat-sender", class: "chat-name" }, player ? playerLabel(player, options.mySeat ?? null) : "");
    };
    const unread = () => messages.filter((m) => m.id > foldedAt && !(options.mySeat && m.seat === options.mySeat)).length;
    const readable = () => state === "open" || state === "disabled_by_opponent";
    const waitLeft = () => Math.max(0, retryUntil - performance.now());

    const reset = () => { messages = []; cursor = 0; };

    const merge = (incoming) => {
      const seen = new Set(messages.map((m) => m.id));
      const fresh = incoming.filter((m) => !seen.has(m.id));
      if (fresh.length) { messages = messages.concat(fresh).sort((a, b) => a.id - b.id); lastNewAt = performance.now(); }
      return fresh.length;
    };

    const read = async () => {
      if (reading || stopped || document.visibilityState === "hidden") return;
      reading = true;
      const myEpoch = epoch;
      const wasReadable = readable();
      const query = cursor > 0 && wasReadable ? `?after_id=${cursor}&limit=${PAGE}` : `?limit=${PAGE}`;
      const controller = new AbortController();
      const abort = window.setTimeout(() => controller.abort(), TIMEOUT_MS);
      const result = await api(`${chatUrl}${query}`, { method: "GET", signal: controller.signal });
      window.clearTimeout(abort);
      reading = false;
      if (myEpoch !== epoch) { schedule(0); return; } // the switch changed meanwhile: read again from the new state
      if (result.ok) {
        fails = 0;
        const data = result.data;
        limits = data.limits;
        opponentDisabled = Boolean(data.opponentDisabled);
        retryUntil = data.retryAfterMs ? performance.now() + data.retryAfterMs : 0;
        const readableNow = data.state === "open" || data.state === "disabled_by_opponent";
        if (!readableNow) reset();
        else if (!wasReadable) { messages = []; cursor = 0; }
        state = data.state;
        if (readableNow) { merge(data.messages); cursor = Math.max(cursor, data.lastId); }
        render();
        // A full page after a cursor means there is more: keep reading at once.
        schedule(readableNow && data.messages.length >= PAGE && query.includes("after_id") ? 0 : undefined);
        return;
      }
      if (result.error.code === "NOT_FOUND") { options.onGone?.(); return; }
      fails += 1;
      render();
      schedule();
    };

    const schedule = (delay) => {
      window.clearTimeout(timer);
      if (stopped) return;
      const idle = performance.now() - lastNewAt > IDLE_AFTER_MS;
      timer = window.setTimeout(read, delay ?? (idle ? IDLE_POLL_MS : POLL_MS));
    };

    document.addEventListener("visibilitychange", () => { if (document.visibilityState === "visible" && !stopped) read(); });

    const send = async (event) => {
      event?.preventDefault();
      if (busy) return;
      const text = draft;
      const count = lengthOf(text);
      if (count === 0) { sendError = gt("chat.rejection.empty"); render(); return; }
      if (count > limits.maxLength) { sendError = gt("chat.rejection.too_long"); render(); return; }
      if (text !== lastText) { clientId = uuid(); lastText = text; }
      busy = true;
      sending = text;
      sendError = null;
      render();
      const sentEpoch = epoch;
      const result = await api(`${chatUrl}`, { body: { text, clientId } });
      busy = false;
      sending = null;
      if (result.ok) {
        draft = "";
        lastText = "";
        clientId = uuid();
        retryUntil = performance.now() + (result.data.retryAfterMs ?? DEFAULT_WAIT_MS);
        if (sentEpoch === epoch) { merge([result.data.message]); cursor = Math.max(cursor, result.data.message.id); stickBottom = true; }
        render(true);
        window.clearTimeout(waitTimer);
        waitTimer = window.setTimeout(() => render(), Math.max(0, retryUntil - performance.now()) + 20);
        return;
      }
      const error = result.error;
      if (error.code === "NOT_FOUND") { options.onGone?.(); return; }
      if (error.code === "network") sendError = { network: true };
      else if (error.code === "RATE_LIMITED") {
        const wait = error.data?.retryAfterMs ?? DEFAULT_WAIT_MS;
        retryUntil = performance.now() + wait;
        sendError = gt("chat.wait");
        window.clearTimeout(waitTimer);
        waitTimer = window.setTimeout(() => { sendError = null; render(); }, wait + 20);
      } else if (error.code === "VALIDATION") sendError = gt(`chat.rejection.${error.data?.reason ?? "not_text"}`);
      else if (error.code === "FORBIDDEN") sendError = gt("chat.readOnly");
      else if (["CHAT_UNAVAILABLE", "CHAT_DISABLED", "CHAT_FULL"].includes(error.code)) { sendError = gt(`chat.refused.${error.code}`); read(); }
      else sendError = gt("chat.offline");
      render();
    };

    const toggleSwitch = async () => {
      if (switchBusy) return;
      const enable = state === "disabled_by_you";
      switchBusy = true;
      switchError = null;
      render();
      const result = await api(`${base}/chat/settings`, { method: "PUT", body: { enabled: enable } });
      switchBusy = false;
      if (result.ok) {
        epoch += 1;
        state = result.data.state;
        if (!readable()) reset(); else { reset(); }
        render();
        read();
        return;
      }
      if (result.error.code === "NOT_FOUND") { options.onGone?.(); return; }
      if (["CHAT_UNAVAILABLE"].includes(result.error.code)) { read(); return; }
      switchError = gt(enable ? "chat.turnOnFailed" : "chat.turnOffFailed");
      render();
    };

    const alertBox = (kind, ...children) => h("p", { class: `alert ${kind}`, role: kind === "danger" ? "alert" : "status" }, ...children);

    const messageItem = (m) => {
      const mine = options.mySeat && m.seat === options.mySeat;
      const when = new Date(m.createdAt);
      return h("li", { "data-testid": "chat-message", class: mine ? "mine" : "" },
        nameOf(m), " ",
        h("time", { datetime: m.createdAt, "aria-hidden": "true" }, when.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })),
        h("p", { dir: "auto", class: "chat-text" }, m.text));
    };

    const list = () => {
      const log = h("div", { role: "log", "aria-live": "polite", "aria-relevant": "additions", "aria-label": gt("chat.log"), tabindex: "0", class: "chat-log", "data-testid": "chat-log" });
      const ol = h("ol", null, messages.map(messageItem));
      if (sending !== null) {
        ol.append(h("li", { "data-testid": "chat-pending", "aria-hidden": "true", class: "pending" },
          h("span", { class: "chat-name" }, spectator ? gt("chat.you") : playerLabel(options.players().find((p) => p.seat === options.mySeat) ?? { seat: options.mySeat, kind: "guest", displayName: "" }, options.mySeat)),
          " ", gt("chat.sending"), h("p", { dir: "auto", class: "chat-text" }, sending)));
      }
      if (messages.length === 0 && sending === null) log.append(h("p", { class: "chat-empty" }, gt("chat.empty")));
      log.append(ol);
      log.addEventListener("scroll", () => { stickBottom = log.scrollHeight - log.scrollTop - log.clientHeight <= 16; });
      return log;
    };

    const composer = () => {
      const blocked = state !== "open" || (spectator && options.canWrite && !options.canWrite());
      const reason = state === "disabled_by_opponent" ? gt("chat.disabledByOpponentReason") : state === null ? gt("chat.loading") : null;
      const input = h("input", {
        type: "text", name: "message", id: "chat-message-input", autocomplete: "off", enterkeyhint: "send", dir: "auto", value: draft,
        disabled: blocked || null, "data-testid": "chat-input",
        oninput: (e) => { draft = e.target.value; sendError = null; updateCounter(); },
      });
      const counter = h("small", { "data-testid": "chat-counter", id: "chat-hint" }, "");
      const button = h("button", { type: "submit", "data-testid": "chat-send", disabled: blocked || busy || waitLeft() > 0 || null }, busy ? gt("chat.sending") : gt("chat.send"));
      const updateCounter = () => {
        const left = limits.maxLength - lengthOf(draft);
        counter.textContent = reason ?? (left >= 0 ? gtp("chat.left", left) : gtp("chat.over", -left));
      };
      updateCounter();
      const err = sendError && typeof sendError === "object"
        ? alertBox("danger", gt("chat.offline"), " ", h("button", { type: "button", "data-testid": "chat-resend", onclick: () => send() }, gt("chat.resend")))
        : sendError ? h("p", { role: "alert", "data-testid": "chat-error" }, sendError) : null;
      return h("form", { onsubmit: send, novalidate: true },
        h("label", { for: "chat-message-input" }, gt("chat.message")), " ", input, " ", button, h("br"), counter, err);
    };

    const body = () => {
      if (state === null) return [h("p", { "data-testid": "chat-loading", "aria-busy": "true" }, gt("chat.loading")), composer()];
      if (state === "closed") return [h("p", null, gt("chat.closed"))];
      if (state === "unavailable") return [h("p", null, options.unavailableText ?? gt("chat.waiting"))];
      if (state === "disabled_by_you") return [h("p", { role: "status" }, gt("chat.disabledByYou"))];
      const parts = [];
      if (fails >= FAILS_BEFORE_ALERT) parts.push(alertBox("danger", gt("chat.refreshFailed"), " ", h("button", { type: "button", onclick: () => { fails = 0; read(); } }, gt("chat.retry"))));
      if (opponentDisabled) parts.push(alertBox("info", gt("chat.opponentDisabledPublic")));
      if (state === "disabled_by_opponent") parts.push(alertBox("info", gt("chat.disabledByOpponent")));
      if (spectator && options.canWrite && !options.canWrite()) {
        parts.push(list(), h("p", { "data-testid": "chat-read-only" }, gt("chat.readOnly")));
      } else parts.push(list(), composer());
      return parts;
    };

    let lastSig = "";
    const render = (force = false) => {
      const sig = JSON.stringify([state, messages.length, messages.at(-1)?.id ?? 0, fails, folded, infoOpen, busy, sending, switchBusy, switchError, sendError, opponentDisabled, waitLeft() > 0, foldedAt, options.players().length, limits.maxLength]);
      if (sig === lastSig && !force) return;
      lastSig = sig;
      const hadFocus = document.activeElement?.id === "chat-message-input";
      const previousLog = root.querySelector('[role="log"]');
      const oldScroll = previousLog?.scrollTop ?? 0;
      const count = unread();
      const toggle = h("button", { type: "button", "data-testid": "chat-toggle", "aria-expanded": String(!folded), "aria-controls": "chat-body",
        onclick: () => { folded = !folded; if (folded) foldedAt = messages.length ? messages[messages.length - 1].id : cursor; render(); } },
        folded ? (count > 0 ? gtp("chat.unread", count) : gt("chat.expand")) : gt("chat.collapse"));
      const info = h("button", { type: "button", "data-testid": "chat-info-toggle", "aria-expanded": String(infoOpen), "aria-label": gt("chat.infoLabel"), onclick: () => { infoOpen = !infoOpen; render(); } }, "ⓘ");
      const showSwitch = !spectator && (state === "open" || state === "disabled_by_you" || state === "disabled_by_opponent");
      const sw = showSwitch
        ? h("button", { type: "button", "data-testid": "chat-switch", "aria-busy": String(switchBusy), onclick: toggleSwitch }, state === "disabled_by_you" ? gt("chat.turnOn") : gt("chat.turnOff"))
        : null;
      const withName = spectator ? null : h("span", null, `${gt("chat.with")} `, (() => {
        const other = options.players().find((p) => p.seat !== options.mySeat);
        return h("span", { "data-testid": "chat-sender" }, other ? playerLabel(other, options.mySeat) : "");
      })());
      const heading = h("h2", { id: "chat-title" }, gt("chat.title"), " ", withName);
      const status = h("p", { class: "sr-only", role: "status" }, folded && count > 0 ? gtp("chat.unreadStatus", count) : "");
      root.replaceChildren(...[
        heading, h("div", { class: "chat-head" }, info, " ", sw, " ", toggle), status,
        infoOpen ? h("p", { "data-testid": "chat-info" }, gt(spectator ? "chat.spectatorInfo" : "chat.info")) : null,
        switchError ? alertBox("danger", switchError) : null,
        h("div", { id: "chat-body", hidden: folded || null }, ...(folded ? [] : body())),
      ].filter(Boolean));
      const log = root.querySelector('[role="log"]');
      if (log) log.scrollTop = stickBottom ? log.scrollHeight : oldScroll;
      if (hadFocus) root.querySelector("#chat-message-input")?.focus();
    };

    return {
      el: root,
      start() { if (!stopped) return; stopped = false; render(); read(); },
      stop() { stopped = true; window.clearTimeout(timer); },
      /** For rooms that show only a sentence (a waiting game). */
      sentence(text) { stopped = true; root.replaceChildren(h("h2", { id: "chat-title" }, gt("chat.title")), h("p", null, text)); },
    };
  };

  window.GomokuChat = { createChat };
})();
