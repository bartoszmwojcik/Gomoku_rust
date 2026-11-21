// F0 spike: alternate black/white stones locally to prove that the board is usable without a framework.
(() => {
  let turn = "black";
  document.querySelector(".board")?.addEventListener("click", (event) => {
    const cell = event.target.closest(".cell");
    if (!cell || cell.dataset.stone) return;
    cell.dataset.stone = turn;
    turn = turn === "black" ? "white" : "black";
  });
})();
