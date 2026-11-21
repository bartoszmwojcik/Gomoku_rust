// Generates node.json: outcomes of the Node.js reference implementation, for the Rust parity test (golden_node.rs).
// Run from the root of the reference repo (it has node_modules): cp this file there, then
//   npx tsx generate.ts <path-to>/crates/gomoku-core/tests/golden/node.json
import { writeFileSync } from "node:fs";
import { applyMove, type MoveState } from "./packages/core/src/games/apply-move";
import { BOARD_CELLS, createBitboard } from "./packages/core/src/games/board";
import { normalizeNickname, nicknameSchema } from "./packages/core/src/games/nickname";
import { extractInvitationCode } from "./packages/core/src/games/invitation-code";

function mulberry32(seed: number) {
  let a = seed;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
const random = mulberry32(424242);
const games: unknown[] = [];
for (let g = 0; g < 300; g++) {
  const blackSeat = (g % 2 === 0 ? 1 : 2) as 1 | 2;
  const order = Array.from({ length: BOARD_CELLS }, (_, c) => c)
    .map((c) => ({ c, k: random() * (4 + Math.hypot((c % 15) - 7, Math.floor(c / 15) - 7)) }))
    .sort((a, b) => a.k - b.k)
    .map(({ c }) => c);
  let state: MoveState = { black: createBitboard(), white: createBitboard(), moveCount: 0, blackSeat };
  const moves: number[] = [];
  let outcome: unknown = null;
  for (const cell of order) {
    const r = applyMove(state, cell);
    if (!r.ok) throw new Error("illegal");
    state = r.state;
    moves.push(cell);
    if (r.result) { outcome = { winnerSeat: r.result.winnerSeat, reason: r.result.reason, winLine: r.winLine }; break; }
  }
  games.push({ blackSeat, moves, outcome });
}
const nicknameInputs = ["Ala", "  Ala  Ola\t", "a‮b​c", "é", "Zażółć gęślą", "Admin", "Ad min", "a.b", "", "   ", "x".repeat(25), "x".repeat(24),
  "\u{20BB7}".repeat(24), "क्‍ष", "ok\u{1F600}", "a  b", "Usunięty gracz", "Sys‌tem", "\u{10400}", "a b", "ﬁ", "Å", "a\u0000b"];
const nicknames = nicknameInputs.map((input) => {
  const r = nicknameSchema.safeParse(input);
  return { input, normalized: normalizeNickname(input), ok: r.success, value: r.success ? (r.data ?? null) : null,
    code: r.success ? null : ((r.error.issues[0] as { params?: { code?: string } }).params?.code ?? null) };
});
const codeInputs = ["ABCD-EFGH", " ab cd efgh ", "https://x/invite/abcd-efgh/", "https://x/invite/ABCD-EFGH?q=1#f", "ftp://x/invite/ABCD-EFGH", "ABCDEFG0", "ß", "straße12", "﻿ABCDEFGH", "https://x/", "http://h:99/a//ABCD-EFGH//"];
const codes = codeInputs.map((input) => ({ input, code: extractInvitationCode(input) }));
writeFileSync(process.argv[2]!, JSON.stringify({ games, nicknames, codes }));
console.log(games.length, nicknames.length, codes.length);
