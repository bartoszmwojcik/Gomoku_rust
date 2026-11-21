// The e-mail confirmation reminder: nothing is blocked by it. Closing it is remembered in this browser for 7 days
// (key per account, no e-mail or name in it); without localStorage it simply shows on every visit. The server
// always renders it open, so nobody who closed it gets a layout shift from a late-appearing banner.
(() => {
  const banner = document.querySelector("[data-verify-banner]");
  if (!banner) return;
  const key = `gomoku_verify_banner_closed:${banner.dataset.user}`;
  const DAY = 24 * 60 * 60 * 1000;
  try {
    const closedAt = Number(window.localStorage.getItem(key));
    if (closedAt && Date.now() - closedAt < 7 * DAY) banner.hidden = true;
  } catch { /* storage unavailable: stay visible */ }
  banner.querySelector("[data-banner-close]")?.addEventListener("click", () => {
    banner.hidden = true;
    try { window.localStorage.setItem(key, String(Date.now())); } catch { /* ignore */ }
  });
})();
