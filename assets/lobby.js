// The lobby: mark yourself as looking for a game, see the others, pick one. Times are relative and counted down locally.
(() => {
  const { gt, gtp } = window.GT;
  const { api, h } = window.Gomoku;
  const root = document.getElementById("lobby");
  const POLL_MS = 3000;
  const READ_TIMEOUT_MS = 8000;
  const FAILURES_BEFORE_ERROR = 2;

  let me = null;
  let currentGame = null;
  let seekers = [];
  let stamp = performance.now();
  let problem = null;
  let failures = 0;
  let version = 0;
  let busy = false;
  let expiredNotice = false;
  let knowsGame = false;
  let timer = null;
  let reading = false;

  const take = (view) => {
    me = view.me;
    currentGame = view.currentGame;
    seekers = view.seekers;
    stamp = performance.now();
    if (currentGame && !knowsGame) { window.location.assign(`/games/${currentGame.gameId}`); }
    knowsGame = Boolean(currentGame);
  };
  take(JSON.parse(document.getElementById("lobby-initial").textContent));
  knowsGame = true;

  const elapsed = () => performance.now() - stamp;
  const left = (ms) => ms - elapsed();
  const minutes = (ms) => `${Math.floor(Math.max(0, ms) / 60000)}:${String(Math.floor((Math.max(0, ms) % 60000) / 1000)).padStart(2, "0")}`;

  const read = async () => {
    if (reading || document.visibilityState === "hidden") return;
    reading = true;
    const seq = version;
    const controller = new AbortController();
    const abortTimer = window.setTimeout(() => controller.abort(), READ_TIMEOUT_MS);
    const result = await api("/api/lobby", { method: "GET", signal: controller.signal });
    window.clearTimeout(abortTimer);
    reading = false;
    if (seq !== version) return; // an own action happened meanwhile: this answer may predate it
    if (result.ok) {
      const hadStatus = me !== null;
      failures = 0;
      take(result.data);
      if (hadStatus && me === null && !currentGame) expiredNotice = true;
    } else failures += 1;
    render();
  };
  const tick = () => { if (document.visibilityState !== "hidden") read(); timer = window.setTimeout(tick, POLL_MS); };
  document.addEventListener("visibilitychange", () => { if (document.visibilityState === "visible") read(); });

  const action = async (call, onOk) => {
    if (busy) return;
    busy = true;
    version += 1;
    problem = null;
    expiredNotice = false;
    render();
    const result = await call();
    busy = false;
    version += 1;
    if (result.ok) onOk(result.data);
    else problem = result.error;
    render();
    return result;
  };

  const mark = () => action(() => api("/api/lobby/seeking", { method: "PUT" }), (data) => { me = data.me; stamp = performance.now(); read(); });
  const withdraw = () => action(() => api("/api/lobby/seeking", { method: "DELETE" }), () => { me = null; });
  const pick = async (seeker) => {
    const result = await action(() => api(`/api/lobby/seekers/${seeker.id}/pick`), (data) => { window.location.assign(`/games/${data.gameId}`); });
    if (result && !result.ok) read();
  };

  const alertBox = (kind, ...children) => h("section", { class: `alert ${kind}`, role: kind === "danger" ? "alert" : "status" }, ...children);

  const problemBox = () => {
    if (!problem) return null;
    const code = problem.code;
    if (code === "ACTIVE_GAME_EXISTS" && problem.data?.gameId) {
      currentGame = { gameId: problem.data.gameId, status: "active" };
      knowsGame = true;
      return null;
    }
    if (code === "RATE_LIMITED") return alertBox("danger", h("strong", null, gt("common.rateLimited.title")), " ",
      problem.retryAfterSeconds ? gt("common.rateLimited.inMinutes", { minutes: Math.max(1, Math.ceil(problem.retryAfterSeconds / 60)) }) : gt("common.rateLimited.later"));
    if (code === "UNAUTHORIZED") return alertBox("warning", h("strong", null, gt("lobby.sessionExpired")), " ", gt("lobby.signedOut", { link: "" }), h("a", { href: "/login?next=%2Flobby" }, gt("lobby.loginAgain")));
    if (code === "network") return alertBox("danger", h("strong", null, gt("lobby.networkTitle")), " ", gt("lobby.networkBody"));
    if (code === "LOBBY_SEEKER_TAKEN" || code === "CANNOT_PICK_SELF") return alertBox("warning", h("strong", null, gt("lobby.takenTitle")), " ", gt("lobby.takenBody"));
    if (code === "LOBBY_SEEKER_EXPIRED") return alertBox("info", h("strong", null, gt("lobby.expiredTitle")), " ", gt("lobby.seekerExpired"));
    return alertBox("danger", h("strong", null, gt(problem.pickFailed ? "lobby.pickFailedTitle" : "lobby.markFailedTitle")), " ", gt("lobby.markFailedBody"));
  };

  const render = () => {
    const box = problemBox();
    const parts = [];
    if (currentGame) {
      parts.push(alertBox("warning", h("strong", null, gt("lobby.activeTitle")), " ", gt("lobby.activeBody")),
        h("p", null, h("a", { href: `/games/${currentGame.gameId}`, "data-testid": "lobby-own-game" }, gt("lobby.backToOwn"))));
    } else {
      if (box) parts.push(box);
      const status = me && left(me.expiresInMs) > 0 ? me : null;
      if (me && !status) { me = null; expiredNotice = true; }
      if (expiredNotice) parts.push(alertBox("info", h("strong", null, gt("lobby.expiredTitle")), " ", gt("lobby.markAgain")));
      parts.push(status
        ? h("section", { "data-testid": "lobby-seeking" }, h("h2", null, gt("lobby.seekingTitle")),
          h("p", { role: "status" }, `${gt("lobby.seekingExpires")} `, h("span", { "data-expires": status.expiresInMs }, minutes(left(status.expiresInMs)))),
          h("button", { type: "button", disabled: busy, onclick: withdraw }, gt("lobby.withdraw")))
        : h("section", { "data-testid": "lobby-mark" }, h("button", { type: "button", disabled: busy, "data-testid": "mark", onclick: mark }, gt("lobby.mark"))));
      const live = seekers.filter((s) => left(s.expiresInMs) > 0);
      const list = failures >= FAILURES_BEFORE_ERROR
        ? alertBox("danger", h("strong", null, gt("lobby.loadFailedTitle")), " ", gt("lobby.loadFailedBody"), " ", h("button", { type: "button", onclick: () => { failures = 0; read(); } }, gt("lobby.retry")))
        : live.length === 0
          ? h("div", { "data-testid": "lobby-empty" }, h("strong", null, gt("lobby.emptyTitle")), " ", gt(status ? "lobby.emptySeeking" : "lobby.emptyIdle"))
          : h("ul", { class: "seekers", "data-testid": "seekers" }, live.map((s) => {
            const since = s.sinceMs + elapsed();
            const sinceText = since < 60000 ? gt("lobby.sinceNow") : gt("lobby.sinceMinutes", { minutes: Math.floor(since / 60000) });
            return h("li", { "data-seeker": s.id },
              h("strong", { "data-testid": "seeker-name" }, s.displayName), " ",
              gt("lobby.rowSince", { settings: gtp("home.settings.classicRules", 10), since: sinceText }), " · ",
              `${gt("lobby.rowExpires")} `, h("span", { "data-expires": s.expiresInMs }, minutes(left(s.expiresInMs))), " ",
              h("button", { type: "button", disabled: busy, "data-testid": "pick", "aria-label": gt("lobby.playWith", { name: s.displayName }), onclick: () => pick(s) }, gt("lobby.play")));
          }));
      parts.push(h("section", null, h("h2", null, gt("lobby.listTitle")), list));
    }
    parts.push(h("p", null, h("a", { href: "/" }, gt("lobby.backHome"))));
    root.replaceChildren(...parts);
  };

  render();
  window.setInterval(() => {
    // A status that ran out locally must disappear without waiting for the next read.
    if ((me && left(me.expiresInMs) <= 0) || seekers.some((s) => left(s.expiresInMs) <= 0)) { render(); read(); return; }
    root.querySelectorAll("[data-expires]").forEach((el) => { el.textContent = minutes(left(Number(el.dataset.expires))); });
  }, 1000);
  timer = window.setTimeout(tick, POLL_MS);
})();
