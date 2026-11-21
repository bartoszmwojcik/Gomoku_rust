// Progressive enhancement: a form with data-api sends its fields as JSON and shows server errors in place.
// Server error codes are translated in the browser from the catalog embedded in the page (#i18n-errors).
(() => {
  const read = (id) => {
    try { return JSON.parse(document.getElementById(id)?.textContent || "{}"); } catch { return {}; }
  };
  const errors = read("i18n-errors");
  const texts = document.getElementById("i18n-form")?.dataset ?? {};

  const translate = (code, reason, fallback) => errors[`${code}.${reason}`] ?? errors[code] ?? fallback;

  const showErrors = (form, error) => {
    form.querySelectorAll("[data-error-for]").forEach((el) => { el.textContent = ""; });
    const slot = form.querySelector("[data-form-error]");
    if (slot) slot.textContent = translate(error.code, error.data?.reason, error.message);
    const fields = error.fieldErrors ?? {};
    for (const [name, messages] of Object.entries(fields)) {
      const el = form.querySelector(`[data-error-for="${name}"]`);
      if (!el) continue;
      const issues = error.fieldIssues?.[name] ?? [];
      el.textContent = messages.map((m, i) => errors[issues[i]?.code] ?? m).join(" ");
    }
    if (error.fieldErrors && slot && error.code === "VALIDATION") slot.textContent = "";
  };

  document.addEventListener("submit", async (event) => {
    const form = event.target.closest?.("form[data-api]");
    if (!form) return;
    event.preventDefault();
    const submit = form.querySelector('button[type="submit"]');
    const body = {};
    for (const el of form.elements) if (el.name) body[el.name] = el.value;
    if (submit) submit.disabled = true;
    const slot = form.querySelector("[data-form-error]");
    if (slot) slot.textContent = texts.sending ?? "";
    try {
      const response = await fetch(form.dataset.api, {
        method: "POST",
        headers: { "Content-Type": "application/json", "X-Gomoku-Request": "1" },
        body: JSON.stringify(body),
        credentials: "same-origin",
      });
      const result = await response.json().catch(() => null);
      if (response.ok && result?.ok) {
        window.location.assign(form.dataset.redirect || "/");
        return;
      }
      showErrors(form, result?.error ?? { code: "INTERNAL", message: texts.unexpected ?? "" });
    } catch {
      if (slot) slot.textContent = texts.network ?? "";
    } finally {
      if (submit) submit.disabled = false;
    }
  });
})();
