// The game settings picker: the variant and, for paper and pencil, the time per player. It fills `[data-settings]`
// containers with radio inputs named `variant` and `timeControlMs`, so the forms pick them up; the classic game sends
// nothing at all. A `[data-rules]` element tells what is played.
(() => {
  const TIMES = [60000, 120000, 180000, 300000];
  const DEFAULT_TIME = 180000;

  const mount = (container, { onChange } = {}) => {
    const { gt, gtp } = window.GT;
    let variant = "classic";
    let time = DEFAULT_TIME;
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
    };

    const render = () => {
      const groups = [
        el("fieldset", { "data-testid": "settings-variant" },
          el("legend", {}, gt("home.settings.variant")),
          radio("variant", "classic", gt("home.settings.variants.classic"), variant === "classic", "settings-variant-classic", () => { variant = "classic"; changed(); }),
          radio("variant", "paper", gt("home.settings.variants.paper"), variant === "paper", "settings-variant-paper", () => { variant = "paper"; changed(); })),
      ];
      if (variant === "paper") {
        groups.push(el("fieldset", { "data-testid": "settings-time" },
          el("legend", {}, gt("home.settings.time")),
          ...TIMES.map((ms) => radio("timeControlMs", String(ms), gt("home.settings.minutes", { minutes: ms / 60000 }), ms === time, `settings-time-${ms}`, () => { time = ms; changed(); }, { "data-json": "int" }))));
      }
      container.replaceChildren(...groups);
      rules();
    };
    const changed = () => { render(); onChange?.({ variant, time }); };
    container.classList.add("settings");
    render();
    return { get: () => (variant === "paper" ? { variant, timeControlMs: time } : {}) };
  };

  document.addEventListener("DOMContentLoaded", () => {
    document.querySelectorAll("[data-settings]").forEach((c) => { if (!c.dataset.mounted) { c.dataset.mounted = "1"; mount(c); } });
  });
  window.GomokuSettings = { mount, TIMES };
})();
