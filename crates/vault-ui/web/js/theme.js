/* Theme loader. Runs in <head> before the page paints, so there is no flash of the wrong theme. */
(function () {
  var KEY = "vault.theme";
  var THEMES = [
    { id: "auto", name: "Match system", note: "Light or dark" },
    { id: "light", name: "Light", note: "The default" },
    { id: "dark", name: "Dark", note: "Easy on the eyes" },
    { id: "sand", name: "Sand", note: "Warm paper" },
    { id: "ocean", name: "Ocean", note: "Cool blue-grey" },
  ];
  var mq = window.matchMedia ? window.matchMedia("(prefers-color-scheme: dark)") : null;
  function pref() {
    try {
      var v = localStorage.getItem(KEY);
      if (THEMES.some(function (t) { return t.id === v; })) return v;
    } catch (_) {}
    return "auto";
  }
  function apply() {
    var p = pref();
    var t = p === "auto" ? (mq && mq.matches ? "dark" : "light") : p;
    document.documentElement.setAttribute("data-theme", t);
    document.documentElement.setAttribute("data-theme-pref", p);
  }
  function set(id) {
    try { localStorage.setItem(KEY, id); } catch (_) {}
    apply();
  }
  if (mq && mq.addEventListener) mq.addEventListener("change", apply);
  window.VaultTheme = { themes: THEMES, get: pref, set: set };
  apply();
})();
