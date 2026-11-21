// The code entry form: only the SHAPE is checked here (8 symbols); what the code means is decided by the invitation page.
(() => {
  const form = document.getElementById("join-form");
  const input = document.getElementById("join-code");
  const error = document.getElementById("join-error");
  if (!form || !input) return;
  const reset = () => { error.textContent = ""; };
  window.addEventListener("pageshow", (event) => { if (event.persisted) reset(); });
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    const code = input.value.replace(/[\s-]+/g, "").toUpperCase();
    if (code.length !== 8) {
      error.textContent = window.GT.gt("home.join.invalid", { length: 8 });
      input.focus();
      return;
    }
    window.location.assign(`/invite/${code.slice(0, 4)}-${code.slice(4)}`);
  });
})();
