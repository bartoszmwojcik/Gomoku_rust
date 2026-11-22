// The review of a finished game: step through the moves; no request is made while stepping.
(() => {
  const { gt } = window.GT;
  const { h, stonesOf, createBoard, outcomeOf, neutralOutcomeOf, playerLabel, formatDate, cellParts } = window.Gomoku;
  const root = document.getElementById("review");
  const data = JSON.parse(document.getElementById("review-data").textContent);
  const total = data.moves.length;
  let step = total;
  const board = createBoard({ onCell: () => {} });
  const clamp = (n) => Math.min(total, Math.max(0, n));

  const draw = () => {
    const stones = stonesOf(data.moves, step);
    const last = step > 0 ? data.moves[step - 1] : null;
    const finalStep = step === total && data.result?.reason === "five_in_row";
    board.update({ stones, last, winLine: finalStep ? data.result.winLine ?? [] : [], readOnly: true });
    status.textContent = step === 0 ? gt("review.emptyBoard") : gt("review.step", {
      step, total, color: gt(`review.color.${(step - 1) % 2 === 0 ? "black" : "white"}`), ...cellParts(last),
    });
    for (const [id, off] of [["review-start", step === 0], ["review-prev", step === 0], ["review-next", step === total], ["review-end", step === total]]) {
      root.querySelector(`[data-testid="${id}"]`).disabled = off;
    }
  };
  const go = (n) => () => { step = clamp(typeof n === "function" ? n(step) : n); draw(); };
  const status = h("p", { role: "status", "data-testid": "review-step" }, "");
  const outcome = data.result
    ? (data.viewerSeat === null ? neutralOutcomeOf(data.result, data.blackSeat, data.players) : outcomeOf(data.result, data.viewerSeat))
    : null;
  const reason = data.result ? gt(`history.reason.${data.result.reason}`) : "";
  const blackSeat = data.blackSeat;
  root.append(
    outcome ? h("section", { class: `result ${outcome.kind}`, role: "status", "data-testid": "result" }, gt("review.banner", { reason, detail: outcome.title })) : null,
    h("p", { "data-testid": "review-finished-at" }, gt("review.finishedAt", { date: data.finishedAt ? formatDate(data.finishedAt) : "" })),
    h("ul", { "aria-label": gt("review.players"), "data-testid": "review-players" }, data.players.map((p) =>
      h("li", null, gt("review.playerLine", { side: gt(`game.sides.classic.title.${p.seat === blackSeat ? "black" : "white"}`) }), playerLabel(p, data.viewerSeat)))),
    board.el, status,
    h("div", { role: "group", "aria-label": gt("review.steps") },
      h("button", { type: "button", "data-testid": "review-start", onclick: go(0) }, gt("review.start")), " ",
      h("button", { type: "button", "data-testid": "review-prev", onclick: go((n) => n - 1) }, gt("review.previous")), " ",
      h("button", { type: "button", "data-testid": "review-next", onclick: go((n) => n + 1) }, gt("review.next")), " ",
      h("button", { type: "button", "data-testid": "review-end", onclick: go(total) }, gt("review.end"))),
  );
  draw();
})();
