// Progressive enhancement: a form with data-api sends its fields as JSON and shows server errors in place.
// Server error codes are translated in the browser from the catalog embedded in the page (#i18n-errors).
//
// Attributes: data-api (url), data-method, data-redirect (path) | data-redirect-game (to /games/{gameId} from the answer),
// data-success (selector to reveal) + data-hide-on-success + data-reload-on-success,
// data-reveal-<CODE> (selector to reveal when the server answers that error code; a [data-game-link] inside gets the
// game from error.data.gameId), data-refresh-on (space separated codes after which the page is reloaded).
(() => {
  const read = (id) => {
    try { return JSON.parse(document.getElementById(id)?.textContent || "{}"); } catch { return {}; }
  };
  const errors = read("i18n-errors");
  const texts = document.getElementById("i18n-form")?.dataset ?? {};

  const translate = (code, reason, fallback) =>
    (reason && (errors[`${code}.${reason}`] ?? errors[`api.${code}.${reason}`])) ??
    errors[code] ?? errors[`api.${code}`] ?? errors[`api.${code}._`] ?? fallback;

  const issueText = (field, issue, fallback) => {
    const text = errors[issue?.code] ?? errors[`field.${field}.${issue?.code}`];
    return text ? text.replace("{count}", issue?.params?.max ?? 24) : fallback;
  };

  const showErrors = (form, error) => {
    form.querySelectorAll("[data-error-for]").forEach((el) => { el.textContent = ""; });
    const slot = form.querySelector("[data-form-error]");
    if (slot) slot.textContent = translate(error.code, error.data?.reason, error.message);
    const fields = error.fieldErrors ?? {};
    for (const [name, messages] of Object.entries(fields)) {
      const el = form.querySelector(`[data-error-for="${name}"]`);
      if (!el) continue;
      const issues = error.fieldIssues?.[name] ?? [];
      el.textContent = messages.map((m, i) => issueText(name, issues[i], m)).join(" ");
    }
    if (error.fieldErrors && slot && error.code === "VALIDATION") slot.textContent = "";
    for (const attr of [...form.attributes]) {
      if (!attr.name.startsWith("data-reveal-") || attr.name.slice(12).toUpperCase() !== error.code) continue;
      const target = document.querySelector(attr.value);
      if (!target) continue;
      target.hidden = false;
      const gameId = error.data?.gameId;
      target.querySelectorAll("[data-game-link]").forEach((a) => { if (gameId) a.href = `/games/${gameId}`; });
      if (slot) slot.textContent = "";
    }
  };

  document.addEventListener("submit", async (event) => {
    const form = event.target.closest?.("form[data-api]");
    if (!form) return;
    event.preventDefault();
    const submit = form.querySelector('button[type="submit"]');
    const body = {};
    for (const el of form.elements) {
      if (!el.name) continue;
      if (el.type === "radio") { if (el.checked) body[el.name] = el.dataset.json === "int" ? Number(el.value) : el.value; continue; }
      if (el.type === "checkbox") body[el.name] = el.checked;
      else if (el.dataset.json === "bool") body[el.name] = el.value === "true";
      else body[el.name] = el.value;
    }
    // The classic game sends no settings at all (old requests stay byte-identical).
    if (body.variant === "classic") { delete body.variant; delete body.timeControlMs; }
    // Against a person nothing about the bot is sent; a bot game is always private.
    if (body.opponent !== "bot") { delete body.opponent; delete body.botLevel; } else delete body.isPublic;
    if (submit) submit.disabled = true;
    const slot = form.querySelector("[data-form-error]");
    if (slot) slot.textContent = texts.sending ?? "";
    try {
      const response = await fetch(form.dataset.api, {
        method: form.dataset.method || "POST",
        headers: { "Content-Type": "application/json", "X-Gomoku-Request": "1" },
        body: JSON.stringify(body),
        credentials: "same-origin",
      });
      const result = await response.json().catch(() => null);
      if (response.ok && result?.ok) {
        if (form.hasAttribute("data-redirect-game")) {
          window.location.assign(`/games/${result.data.gameId}`);
          return;
        }
        if (form.dataset.success) {
          // Stay on the page: show the confirmation, clear the fields (and hide the form when asked to).
          form.reset();
          form.querySelectorAll("[data-error-for], [data-form-error]").forEach((el) => { el.textContent = ""; });
          const done = document.querySelector(form.dataset.success);
          if (done) done.hidden = false;
          if (form.hasAttribute("data-hide-on-success")) form.hidden = true;
          if (form.hasAttribute("data-reload-on-success")) window.setTimeout(() => window.location.reload(), 800);
          return;
        }
        window.location.assign(form.dataset.redirect || "/");
        return;
      }
      const error = result?.error ?? { code: "INTERNAL", message: texts.unexpected ?? "" };
      if ((form.dataset.refreshOn ?? "").split(" ").includes(error.code)) {
        window.location.reload();
        return;
      }
      showErrors(form, error);
    } catch {
      if (slot) slot.textContent = texts.network ?? "";
    } finally {
      if (submit) submit.disabled = false;
    }
  });
})();
