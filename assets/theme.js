// The theme switch: the attribute on <html> is the single source of truth (the head script sets it before the first
// paint); the choice is kept in a cookie for a year. The button names the theme it switches TO.
(() => {
  const root = document.documentElement;
  const button = document.querySelector("[data-theme-toggle]");
  if (!button) return;
  const current = () => (root.getAttribute("data-theme") === "light" ? "light" : "dark");
  const label = () => {
    const target = current() === "light" ? "dark" : "light";
    button.textContent = button.dataset[target === "light" ? "labelLight" : "labelDark"];
    button.setAttribute("aria-pressed", String(current() === "light"));
  };
  button.addEventListener("click", () => {
    const next = current() === "light" ? "dark" : "light";
    root.setAttribute("data-theme", next);
    try {
      document.cookie = `gomoku_theme=${next}; Path=/; Max-Age=31536000; SameSite=Lax${location.protocol === "https:" ? "; Secure" : ""}`;
    } catch { /* the choice holds until the end of the visit */ }
    label();
  });
  label();
})();
