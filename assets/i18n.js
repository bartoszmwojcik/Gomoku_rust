// Texts for the scripts that render screens in the browser: the page embeds the needed catalog entries as JSON.
(() => {
  let bag = {};
  try { bag = JSON.parse(document.getElementById("i18n-bag")?.textContent || "{}"); } catch { /* none */ }
  const lang = document.documentElement.lang || "pl";
  const plural = new Intl.PluralRules(lang);
  const fill = (text, params) => {
    let out = text;
    for (const [name, value] of Object.entries(params ?? {})) out = out.split(`{${name}}`).join(String(value));
    return out;
  };
  /** The text of `key` with `{name}` placeholders filled; an unknown key is the key itself. */
  const gt = (key, params) => fill(bag[key] ?? key, params);
  /** A counted phrase: `key.one|few|many|other` by the language's plural rule. */
  const gtp = (key, count, params) => {
    const specific = `${key}.${plural.select(count)}`;
    return gt(specific in bag ? specific : `${key}.other`, { count, ...params });
  };
  window.GT = { gt, gtp, has: (key) => key in bag, lang };
})();
