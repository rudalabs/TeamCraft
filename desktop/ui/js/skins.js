// SPDX-License-Identifier: MPL-2.0
// TeamCraft Desktop – Skins.
// Part of the desktop UI (ES modules, no build step; js/main.js starts it).

import { TAURI, state } from "./core.js";
import { renderActive } from "./tabs.js";

// ======================================================================
// Skins
// ======================================================================

export function applySkin(id) {
  var root = document.documentElement;
  root.setAttribute("data-skin", id || "teamcraft");
  var light = getComputedStyle(root).colorScheme.indexOf("light") !== -1;
  root.classList.toggle("skin-light", light);
  // The native window chrome (the GTK menu bar on Linux) follows the skin:
  // a dark skin gets the dark variant; the System skin leaves it to the OS.
  var win = TAURI && TAURI.window && TAURI.window.getCurrentWindow ? TAURI.window.getCurrentWindow() : null;
  if (win && win.setTheme) {
    var system = !id || id === "teamcraft";
    win.setTheme(system ? null : (light ? "light" : "dark")).catch(function () {});
  }
  if (state.viewer) renderActive(); // the 3D view takes its colours from the skin
}

// The System skin follows the OS: re-apply it when the OS switches between
// light and dark (WebKitGTK, WebView2 and WKWebView all report it).
if (window.matchMedia) {
  var schemeQuery = window.matchMedia("(prefers-color-scheme: dark)");
  if (schemeQuery.addEventListener) {
    schemeQuery.addEventListener("change", function () {
      applySkin(document.documentElement.getAttribute("data-skin"));
    });
  }
}
