// The game settings picker: the variant and, for paper and pencil, the time per player. It fills `[data-settings]`
// containers with radio inputs named `variant` and `timeControlMs`, so the forms pick them up; the classic game sends
// nothing at all. A `[data-rules]` element tells what is played. A container with `data-bot` also offers the opponent
// (a person by invitation, or the bot at level 1 or 2); the bot plays the classic game only. Elements of the form with
// `data-label-bot` swap their text for the bot, `[data-hide-for-bot]` hides, `[data-show-for-bot]` shows.
(() => {
  const TIMES = [60000, 120000, 180000, 300000];
  const DEFAULT_TIME = 180000;

  const mount = (container, { onChange } = {}) => {
    const { gt, gtp } = window.GT;
    let variant = "classic";
    let time = DEFAULT_TIME;
    let opponent = "human";
    let level = 1;
    const withBot = container.hasAttribute("data-bot");
    const el = (tag, attrs = {}, ...children) => {
      const node = document.createElement(tag);
      for (const [k, v] of Object.entries(attrs)) {
        if (v === false || v === null || v === undefined) continue;
        if (k.startsWith("on")) node.addEventListener(k.slice(2), v); else node.setAttribute(k, v === true ? "" : String(v));
      }
      node.append(...children.filter((c) => c !== null && c !== undefined));
      return node;
    };
    const radio = (name, value, label, checked, testid, onclick, extra = {}) => el("label", {},
      el("input", { type: "radio", name, value, checked, "data-testid": testid, "aria-checked": String(checked), onchange: onclick, ...extra }), ` ${label}`);

    const rules = () => {
      const target = document.querySelector("[data-rules]");
      if (!target) return;
      target.textContent = variant === "paper" ? gt("home.settings.paperHome") : gtp("home.settings.classicRules", 10);
      target.hidden = opponent === "bot";
    };

    const sync = () => {
      const form = container.closest("form");
      if (!form) return;
      const bot = opponent === "bot";
      form.querySelectorAll("[data-label-bot]").forEach((el) => {
        if (el.dataset.labelHuman === undefined) el.dataset.labelHuman = el.textContent;
        el.textContent = bot ? el.dataset.labelBot : el.dataset.labelHuman;
      });
      form.querySelectorAll("[data-hide-for-bot]").forEach((el) => { el.hidden = bot; });
      form.querySelectorAll("[data-show-for-bot]").forEach((el) => { el.hidden = !bot; });
    };

    const render = () => {
      const groups = [
        el("fieldset", { "data-testid": "settings-variant" },
          el("legend", {}, gt("home.settings.variant")),
          radio("variant", "classic", gt("home.settings.variants.classic"), variant === "classic", "settings-variant-classic", () => { variant = "classic"; changed(); }),
          radio("variant", "paper", gt("home.settings.variants.paper"), variant === "paper", "settings-variant-paper", () => { variant = "paper"; opponent = "human"; changed(); })),
      ];
      if (withBot) {
        const paper = variant === "paper";
        groups.push(el("fieldset", { "data-testid": "settings-opponent" },
          el("legend", {}, gt("home.opponent.label")),
          radio("opponent", "human", gt("home.opponent.human"), opponent === "human", "settings-opponent-human", () => { opponent = "human"; changed(); }),
          radio("opponent", "bot", gt("home.opponent.bot"), opponent === "bot", "settings-opponent-bot", () => { opponent = "bot"; changed(); }, { disabled: paper }),
          paper ? el("small", { "data-testid": "settings-opponent-hint" }, ` ${gt("home.opponent.classicOnly")}`) : null));
        if (opponent === "bot") {
          groups.push(el("fieldset", { "data-testid": "settings-bot-level" },
            el("legend", {}, gt("home.opponent.levelLabel")),
            ...[1, 2].map((n) => radio("botLevel", String(n), gt("home.opponent.level", { level: n }), level === n, `settings-bot-level-${n}`, () => { level = n; changed(); }, { "data-json": "int" }))));
        }
      }
      if (variant === "paper") {
        groups.push(el("fieldset", { "data-testid": "settings-time" },
          el("legend", {}, gt("home.settings.time")),
          ...TIMES.map((ms) => radio("timeControlMs", String(ms), gt("home.settings.minutes", { minutes: ms / 60000 }), ms === time, `settings-time-${ms}`, () => { time = ms; changed(); }, { "data-json": "int" }))));
      }
      container.replaceChildren(...groups);
      rules();
      sync();
    };
    const changed = () => { render(); onChange?.({ variant, time }); };
    container.classList.add("settings");
    render();
    return { get: () => (variant === "paper" ? { variant, timeControlMs: time } : opponent === "bot" ? { opponent, botLevel: level } : {}) };
  };

  document.addEventListener("DOMContentLoaded", () => {
    document.querySelectorAll("[data-settings]").forEach((c) => { if (!c.dataset.mounted) { c.dataset.mounted = "1"; mount(c); } });
  });
  window.GomokuSettings = { mount, TIMES };
})();
