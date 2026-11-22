// The show console (administrator only): choose a layout, check that the show will start, start it, return to or end the
// one that is running. Errors are alerts, never toasts.
(() => {
  const { gt } = window.GT;
  const { api, h, confirmDialog } = window.Gomoku;

  const root = document.getElementById("show-console");
  const initial = JSON.parse(document.getElementById("show-initial").textContent);

  let overview = initial;
  let layout = initial.botVsBotEnabled ? "bot-bot" : "bot-agent";
  let botLevel = 1;
  let levels = [1, 1];
  let checks = null;
  let checkFailed = false;
  let error = null;
  let busy = false;
  let checking = false;
  let focusTitle = false;

  const megabytes = (mb) => gt("common.bot.megabytes", { mb });
  const alertBox = (kind, ...children) => h("section", { class: `alert ${kind}`, role: kind === "danger" ? "alert" : "status" }, ...children);

  const errorText = (code, reason) => {
    if (code === "SHOW_RUNNING" || code === "RATE_LIMITED") return gt(`show.errors.${code}`);
    if (code === "SHOW_UNAVAILABLE") return gt(`show.errors.SHOW_UNAVAILABLE.${reason === "bot_vs_bot_disabled" ? reason : "disabled"}`);
    if (code === "BOT_UNAVAILABLE") return gt(`show.errors.BOT_UNAVAILABLE.${["disabled", "memory", "busy", "level2_not_allowed"].includes(reason) ? reason : "other"}`);
    return gt("show.errors.other");
  };

  const body = () => (layout === "bot-bot" ? { layout, levels } : { layout, botLevel });
  const blocked = () => overview.current !== null || (layout === "bot-bot" && !overview.botVsBotEnabled);

  const checkRow = (check) => {
    const detail = check.detail ?? {};
    let text;
    if (check.id === "memory") text = gt("show.checks.item.memory", { available: megabytes(detail.availableMb), min: megabytes(detail.minAvailableMb) });
    else if (check.id === "slot") text = gt("show.checks.item.slot", { active: detail.activeGames, max: detail.maxActiveGames });
    else if (check.id === "engine" && check.detail) text = gt("show.checks.item.engineTimed", { ms: detail.ms, limit: detail.limitMs });
    else text = gt(`show.checks.item.${check.id}`);
    const badge = check.ok === null ? gt("show.checks.unknown") : check.ok ? gt("show.checks.pass") : gt("show.checks.fail");
    return h("li", { "data-check": check.id, "data-ok": String(check.ok) }, text, " ", h("span", { class: "badge" }, badge));
  };

  const runCheck = async () => {
    if (checking) return;
    checking = true; checkFailed = false; render();
    const result = await api("/api/show/preflight", { body: body() });
    checking = false;
    if (result.ok) checks = result.data; else { checks = null; checkFailed = true; }
    render();
  };

  const start = async () => {
    if (busy) return;
    busy = true; error = null; render();
    const result = await api("/api/show", { body: body() });
    if (result.ok) { window.location.assign(`/show/${result.data.gameId}`); return; }
    busy = false;
    if (result.error.code === "SHOW_RUNNING" && result.error.data?.gameId) {
      overview = { ...overview, current: { gameId: result.error.data.gameId, layout: result.error.data.layout, status: "active" } };
    }
    error = { code: result.error.code, text: errorText(result.error.code, result.error.data?.reason) };
    focusTitle = true;
    render();
  };

  const endCurrent = () => confirmDialog({
    title: gt("show.end.title"), text: gt("show.end.body"), confirm: gt("show.end.confirm"), cancel: gt("show.end.cancel"), testid: "end-dialog",
    onConfirm: async () => {
      const result = await api(`/api/show/${overview.current.gameId}/end`);
      if (result.ok || result.error.code === "GAME_NOT_ACTIVE") { overview = { ...overview, current: null }; render(); return true; }
      return gt("show.end.failed");
    },
  });

  const levelSelect = (testid, label, value, onChange, level2Allowed) => h("label", null, label, " ",
    h("select", { "data-testid": testid, disabled: blocked(), onchange: (e) => onChange(Number(e.target.value)) },
      [1, 2].map((n) => h("option", { value: n, selected: n === value, disabled: n === 2 && !level2Allowed },
        n === 2 && !level2Allowed ? gt("show.console.levelDisabled", { level: n }) : gt("show.console.level", { level: n })))));

  const render = () => {
    const title = h("h1", { tabindex: "-1" }, gt("show.console.title"));
    if (!overview.enabled) {
      root.replaceChildren(title, h("section", { class: "card" }, h("h2", null, gt("show.console.disabledTitle")), h("p", null, gt("show.console.disabledBody"))));
      return;
    }
    const parts = [title, h("p", null, gt("show.console.lead"))];
    if (error) parts.push(alertBox("danger", h("strong", null, gt("show.errors.createTitle")), " ", error.text, " ", h("small", null, gt("show.errors.code", { code: error.code }))));
    if (overview.current) {
      const current = overview.current;
      parts.push(h("section", { class: "card", "data-testid": "show-running" },
        h("h2", null, gt("show.console.runningTitle")),
        h("p", null, gt("show.console.runningLayout", { layout: gt(`show.layout.${current.layout}`) })),
        h("p", null, gt(`show.console.runningStatus.${current.status === "waiting" ? "waiting" : "active"}`)),
        h("p", null, h("a", { class: "button", href: `/show/${current.gameId}` }, gt("show.console.back")), " ",
          h("button", { type: "button", class: "danger", "data-testid": "show-end", onclick: endCurrent }, gt("show.end.action")))));
    }
    const level2 = layout === "bot-agent" || overview.level2Enabled;
    const form = h("section", { class: "card" }, h("h2", null, gt("show.console.formTitle")),
      h("fieldset", { "data-testid": "show-layout", disabled: overview.current !== null }, h("legend", null, gt("show.console.layoutLabel")),
        ["bot-bot", "bot-agent"].map((value) => h("label", null,
          h("input", { type: "radio", name: "layout", value, checked: layout === value, onchange: () => { layout = value; checks = null; render(); } }), " ", gt(`show.layout.${value}`)))));
    if (layout === "bot-bot" && !overview.botVsBotEnabled) form.append(alertBox("warning", gt("show.console.botVsBotDisabled")));
    if (layout === "bot-agent") {
      form.append(h("p", null, gt("show.console.agentLead")),
        levelSelect("show-bot-level", gt("show.console.botLevelLabel"), botLevel, (n) => { botLevel = n; checks = null; }, true));
    } else {
      form.append(levelSelect("show-level-1", gt("show.console.levelLabel", { seat: 1 }), levels[0], (n) => { levels = [n, levels[1]]; checks = null; }, level2),
        levelSelect("show-level-2", gt("show.console.levelLabel", { seat: 2 }), levels[1], (n) => { levels = [levels[0], n]; checks = null; }, level2));
      if (!overview.level2Enabled) form.append(h("p", null, gt("show.console.level2Blocked")));
    }
    form.append(h("p", null, h("button", { type: "button", "data-testid": "show-check", disabled: checking, onclick: runCheck }, gt("show.console.check"))));
    if (checkFailed) form.append(alertBox("danger", gt("show.checks.failed")));
    if (checks) {
      form.append(h("div", { "data-testid": "show-checks" },
        alertBox(checks.ok ? "success" : "warning", gt(checks.ok ? "show.checks.ready" : "show.checks.notReady")),
        h("ul", { "aria-label": gt("show.checks.title") }, checks.checks.map(checkRow))));
    }
    form.append(h("p", null, h("button", { type: "button", class: "large", "data-testid": "show-start", disabled: blocked() || busy, onclick: start }, gt("show.console.start"))));
    if (overview.current) form.append(h("p", null, gt("show.console.formBlocked")));
    parts.push(form);
    root.replaceChildren(...parts);
    if (focusTitle) { title.focus(); focusTitle = false; }
  };

  render();
})();
