// The history: the first page comes with the page, older ones are fetched on demand (keyset by cursor).
(() => {
  const { gt, gtp } = window.GT;
  const { api, h, formatDate } = window.Gomoku;
  const root = document.getElementById("history");
  const initial = JSON.parse(document.getElementById("history-initial").textContent);
  let entries = [...initial.entries];
  let next = initial.nextCursor;
  let loading = false;
  let problem = null;
  let added = 0;

  const label = (opponent) => {
    if (opponent.kind === "agent") return window.Gomoku.playerLabel({ kind: "agent", seat: 0, agentName: opponent.name }, null);
    if (opponent.kind === "bot") return window.Gomoku.playerLabel({ kind: "bot", seat: 0, displayName: opponent.name, bot: opponent.bot }, null);
    if (opponent.kind === "account") return opponent.name ?? gt("common.player.deleted");
    const match = /^Gracz ([12])$/.exec(opponent.name ?? "");
    const name = match ? gt("common.player.guestDefault", { seat: match[1] }) : opponent.name;
    return gt("common.player.guestLabel", { name });
  };

  const detailOf = (e) => {
    if (e.variant !== "paper") return gt(`history.reason.${e.reason}`);
    const variant = gt("home.settings.variants.paper");
    const score = e.scores ? `${e.scores[0]} : ${e.scores[1]}` : "";
    return e.reason === "points"
      ? gt("history.paperDetail", { variant, score })
      : gt("history.paperDetailWithReason", { variant, score, reason: gt(`history.reason.${e.reason}`) });
  };

  const more = async () => {
    if (loading || !next) return;
    loading = true;
    problem = null;
    render();
    const result = await api(`/api/history?cursor=${encodeURIComponent(next)}`, { method: "GET" });
    loading = false;
    if (result.ok) { entries = entries.concat(result.data.entries); next = result.data.nextCursor; added = result.data.entries.length; }
    else problem = result.error;
    render();
  };

  const render = () => {
    const parts = [];
    if (entries.length === 0) {
      parts.push(h("div", { "data-testid": "history-empty" }, h("strong", null, gt("history.emptyTitle")), " ", gt("history.emptyBody")));
    } else {
      parts.push(h("ul", { class: "history" }, entries.map((e) =>
        h("li", { "data-testid": "history-row" },
          h("strong", { "data-testid": "player-name" }, label(e.opponent)), " · ",
          h("span", { "data-result": e.result }, gt(`history.result.${e.result}`)), " · ",
          detailOf(e), " · ",
          h("time", { datetime: e.finishedAt }, formatDate(e.finishedAt)), " ",
          h("a", { href: `/games/${e.gameId}/review`, "data-testid": "history-review" }, gt("history.review"))))));
    }
    if (problem) {
      parts.push(problem.code === "UNAUTHORIZED"
        ? h("section", { class: "alert warning", role: "alert" }, h("strong", null, gt("history.sessionExpired")), " ", h("a", { href: "/login?next=%2Fhistory" }, gt("history.loginAgain")))
        : h("section", { class: "alert danger", role: "alert" }, h("strong", null, gt("history.moreFailedTitle")), " ", gt("history.moreFailedBody"), " ", h("button", { type: "button", onclick: more }, gt("history.retry"))));
    }
    if (next) parts.push(h("button", { type: "button", "data-testid": "history-more", disabled: loading, onclick: more }, loading ? gt("history.loading") : gt("history.more")));
    parts.push(h("p", { class: "sr-only", role: "status", "aria-live": "polite", "data-testid": "history-added" }, added ? gtp("history.added", added) : ""));
    root.replaceChildren(...parts);
  };
  render();
})();
