// docs.getductus.com's search: Pagefind's modal, in place of mdBook's own search.
//
// Why: mdBook's search downloads one index of the whole book before the first result (5 MB, 0.78 MB
// gzipped: 1.9 s at 4 Mbit/s, 4.5 s on 3G). Pagefind downloads only the pieces a search needs.
// Pagefind can't search from file://, though, so the offline docs zip keeps mdBook's search: this
// script does nothing on a page that has it (architecture.md, "The docs site"; decisions.md "0.6.8.2
// part C answers").
//
// The site is built by scripts/dev/docs-site.sh: mdBook with its search turned off, then Pagefind,
// which writes pagefind/ (the index and its UI) next to the pages. This file only adds the button,
// the modal and the shortcuts.
(function () {
  "use strict";
  if (document.getElementById("searchbar") || location.protocol === "file:") return;
  var root = typeof path_to_root === "string" ? path_to_root : "";
  var bundle = new URL(root + "pagefind/", location.href).pathname;

  var css = document.createElement("link");
  css.rel = "stylesheet";
  css.href = bundle + "pagefind-component-ui.css";
  document.head.appendChild(css);

  var config = document.createElement("pagefind-config");
  config.setAttribute("bundle-path", bundle);
  var trigger = document.createElement("pagefind-modal-trigger");
  trigger.setAttribute("compact", "");
  trigger.setAttribute("hide-shortcut", "");
  trigger.setAttribute("placeholder", "Search the docs");
  var modal = document.createElement("pagefind-modal");
  modal.setAttribute("reset-on-close", "false");

  // Where mdBook puts its own search button: the menu bar's left buttons, after the theme picker.
  var buttons = document.querySelector("#menu-bar .left-buttons");
  if (!buttons) return;
  buttons.appendChild(trigger);
  document.body.appendChild(config);
  document.body.appendChild(modal);

  var ui = document.createElement("script");
  ui.src = bundle + "pagefind-component-ui.js";
  document.head.appendChild(ui);

  // mdBook's keys still open search: S and /, as on any mdBook site (Ctrl+K / Cmd+K is Pagefind's).
  document.addEventListener("keydown", function (e) {
    if (e.ctrlKey || e.metaKey || e.altKey || e.defaultPrevented) return;
    if (e.key !== "s" && e.key !== "S" && e.key !== "/") return;
    var t = e.target;
    if (t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))) return;
    if (typeof modal.open !== "function") return;
    e.preventDefault();
    modal.open();
  });
})();
