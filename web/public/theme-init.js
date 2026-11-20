// Puts the page in dark mode before it is drawn, so it never flashes white first. Loaded as a
// blocking script from index.html; the rest is done by src/lib/theme.ts, which uses the same
// storage key and values.
(function () {
  var theme = null;
  try {
    theme = window.localStorage.getItem("teamtak.theme");
  } catch (e) {
    // Storage blocked: follow the device.
  }
  var dark = theme === "dark" || (theme !== "light" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  if (dark) document.documentElement.classList.add("dark");
})();
