// TeamCraft – progressive enhancements. The UI works without JavaScript.
(function () {
  "use strict";

  // Ask for confirmation on forms marked with data-confirm="…".
  document.addEventListener("submit", function (event) {
    var form = event.target;
    var message = form.getAttribute("data-confirm");
    if (message && !window.confirm(message)) {
      event.preventDefault();
    }
  });

  // A POST form that is on its way is marked busy, so a second click cannot
  // send it twice (CSS: form[aria-busy]). Buttons are not disabled: that
  // would drop the name/value of the clicked button from the request.
  document.addEventListener("submit", function (event) {
    var form = event.target;
    if (form.getAttribute("aria-busy") === "true") { event.preventDefault(); return; }
    if (event.defaultPrevented || form.method !== "post" || form.target) return;
    setTimeout(function () {
      if (event.defaultPrevented) return; // the upload handler took over
      form.setAttribute("aria-busy", "true");
      setTimeout(function () { form.removeAttribute("aria-busy"); }, 8000);
    }, 0);
  });
  // Back/forward cache: a page restored from history is idle again.
  window.addEventListener("pageshow", function () {
    document.querySelectorAll("form[aria-busy]").forEach(function (f) { f.removeAttribute("aria-busy"); });
  });

  // File uploads (attach, check in): progress bar and a "Cancel upload"
  // button that stops the transfer at once. Without JavaScript the form is
  // simply submitted.
  function formatBytes(n) {
    if (n < 1024 * 1024) return Math.max(1, Math.round(n / 1024)) + " KiB";
    return (n / 1024 / 1024).toFixed(1) + " MiB";
  }

  document.querySelectorAll("form[enctype='multipart/form-data']").forEach(function (form) {
    form.addEventListener("submit", function (event) {
      if (event.defaultPrevented || !window.FormData || !window.XMLHttpRequest) return;
      var chosen = Array.prototype.some.call(form.querySelectorAll("input[type=file]"), function (input) {
        return input.files && input.files.length > 0;
      });
      if (!chosen) return; // nothing to transfer: a normal submit is instant
      event.preventDefault();

      var buttons = form.querySelectorAll("button, input[type=submit]");
      var box = document.createElement("div");
      box.className = "upload-progress";
      box.setAttribute("role", "status");
      var bar = document.createElement("progress");
      bar.max = 100;
      bar.value = 0;
      var label = document.createElement("span");
      label.className = "small muted";
      label.textContent = "Uploading …";
      var cancel = document.createElement("button");
      cancel.type = "button";
      cancel.className = "small danger";
      cancel.textContent = "Cancel upload";
      box.appendChild(bar);
      box.appendChild(label);
      box.appendChild(cancel);
      form.appendChild(box);
      Array.prototype.forEach.call(buttons, function (b) { b.disabled = true; });

      function done(message, isError) {
        Array.prototype.forEach.call(buttons, function (b) { b.disabled = false; });
        box.remove();
        if (message) {
          var note = document.createElement("p");
          note.className = isError ? "flash flash-error" : "small muted";
          note.setAttribute("role", "status");
          note.textContent = message;
          form.appendChild(note);
          setTimeout(function () { note.remove(); }, 6000);
        }
      }

      var xhr = new XMLHttpRequest();
      xhr.upload.addEventListener("progress", function (e) {
        if (!e.lengthComputable) return;
        bar.value = Math.round((e.loaded / e.total) * 100);
        label.textContent = "Uploading " + bar.value + " % · " + formatBytes(e.loaded) + " of " + formatBytes(e.total);
      });
      xhr.upload.addEventListener("load", function () {
        // everything is sent: the server stores the files in one transaction,
        // which cannot be cancelled half-way any more
        bar.removeAttribute("value");
        label.textContent = "Storing the files …";
        cancel.remove();
      });
      xhr.addEventListener("load", function () {
        var target = null;
        try { target = JSON.parse(xhr.responseText).location; } catch (e) { /* not JSON */ }
        if (xhr.status === 200 && target) {
          window.location.assign(target);
        } else if (xhr.status === 413) {
          done("The upload exceeds the size limit of this server.", true);
        } else {
          done("The upload failed (" + xhr.status + "). Please try again.", true);
        }
      });
      xhr.addEventListener("error", function () { done("The upload failed – check the connection and try again.", true); });
      xhr.addEventListener("abort", function () { done("Upload cancelled – nothing was changed.", false); });
      cancel.addEventListener("click", function () { xhr.abort(); });

      xhr.open("POST", form.action);
      xhr.setRequestHeader("X-TeamCraft-Script", "1");
      xhr.send(new FormData(form));
    });
  });

  // Show timestamps in the viewer's local time zone.
  var fmt;
  try {
    fmt = new Intl.DateTimeFormat(undefined, {
      year: "numeric", month: "2-digit", day: "2-digit",
      hour: "2-digit", minute: "2-digit"
    });
  } catch (e) {
    fmt = null;
  }
  if (fmt) {
    document.querySelectorAll("time[datetime]").forEach(function (el) {
      var d = new Date(el.getAttribute("datetime"));
      if (!isNaN(d.getTime())) {
        el.title = el.textContent;
        el.textContent = fmt.format(d);
      }
    });
  }

  // "Back" links: <a href="/" data-back> go back in history when possible.
  document.querySelectorAll("[data-back]").forEach(function (link) {
    link.addEventListener("click", function (event) {
      if (window.history.length > 1) {
        event.preventDefault();
        window.history.back();
      }
    });
  });

  // Assistant ("Ask TeamCraft"): the answer can take a minute. Show that it
  // is working and ignore further clicks; the buttons stay enabled so the
  // clicked quick action is still submitted with the form.
  document.querySelectorAll("form.assistant-form").forEach(function (form) {
    form.addEventListener("submit", function (event) {
      if (form.classList.contains("busy")) { event.preventDefault(); return; }
      form.classList.add("busy");
      form.setAttribute("aria-busy", "true");
      var status = form.querySelector(".assistant-status");
      if (status) status.textContent = "Looking it up in TeamCraft … this can take a minute.";
    });
  });
  // Coming back through the history: the form is ready again.
  window.addEventListener("pageshow", function (event) {
    if (!event.persisted) return;
    document.querySelectorAll("form.assistant-form.busy").forEach(function (form) {
      form.classList.remove("busy");
      form.removeAttribute("aria-busy");
      var status = form.querySelector(".assistant-status");
      if (status) status.textContent = "";
    });
  });

  // Toggle elements: <button data-toggle="id">.
  document.querySelectorAll("[data-toggle]").forEach(function (btn) {
    btn.addEventListener("click", function () {
      var target = document.getElementById(btn.getAttribute("data-toggle"));
      if (target) {
        target.classList.toggle("hidden");
        if (btn.hasAttribute("aria-expanded")) btn.setAttribute("aria-expanded", String(!target.classList.contains("hidden")));
        var focus = target.querySelector("input:not([type=hidden]), textarea, select");
        if (focus && !target.classList.contains("hidden")) focus.focus();
      }
    });
  });

  // New item: name and type from the first chosen file
  // (<input type="file" data-fill-name="id" data-fill-type="id">).
  var ASSEMBLY = ["iam", "asm", "sldasm", "catproduct", "f3z"];
  var DRAWING = ["idw", "dwg", "dxf", "dft", "slddrw", "catdrawing", "drw", "ipn", "pdf"];
  document.querySelectorAll("input[type=file][data-fill-name]").forEach(function (input) {
    var name = document.getElementById(input.getAttribute("data-fill-name"));
    var type = document.getElementById(input.getAttribute("data-fill-type"));
    // A type other than the default came with the page (/items/new?type=simulation,
    // a form shown again after an error): keep it instead of guessing.
    var typeChosen = !!(type && type.value && type.value !== "part");
    if (type) type.addEventListener("change", function () { typeChosen = true; });
    input.addEventListener("change", function () {
      var file = input.files && input.files[0];
      if (!file) return;
      // Creo iterations: bracket.prt.7 → bracket.prt
      var base = file.name.replace(/\.\d+$/, "");
      var dot = base.lastIndexOf(".");
      var stem = dot > 0 ? base.slice(0, dot) : base;
      var ext = dot > 0 ? base.slice(dot + 1).toLowerCase() : "";
      if (name && !name.value.trim()) name.value = stem;
      if (type && !typeChosen) {
        type.value = ASSEMBLY.indexOf(ext) >= 0 ? "assembly" : DRAWING.indexOf(ext) >= 0 ? "document" : "part";
      }
    });
  });

  // Preview images that turn out to be missing are hidden, not shown broken
  // (<img data-hide-broken>, optionally inside an a.thumb-link).
  document.querySelectorAll("img[data-hide-broken]").forEach(function (img) {
    function hide() { (img.closest("a.thumb-link") || img).classList.add("hidden"); }
    if (img.complete && img.naturalWidth === 0 && img.getAttribute("loading") !== "lazy") hide();
    img.addEventListener("error", hide);
  });

  // Navigation: the active entry is the current page for screen readers.
  document.querySelectorAll(".nav a.active, .nav-admin a.active").forEach(function (a) {
    a.setAttribute("aria-current", "page");
  });

  // Forms that return to this page (the Appearance choice in the user menu).
  document.querySelectorAll("input[data-here]").forEach(function (input) {
    input.value = location.pathname + location.search;
  });

  // "/" focuses the search field (the page's own filter first), unless the
  // user is typing somewhere.
  document.addEventListener("keydown", function (event) {
    if (event.key !== "/" || event.ctrlKey || event.metaKey || event.altKey) return;
    var t = event.target;
    if (t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))) return;
    var field = document.querySelector("[data-filter]") || document.querySelector(".quicksearch input");
    if (field) { event.preventDefault(); field.focus(); field.select(); }
  });

  // Menus (<details class="menu">): one open at a time, Esc closes and
  // returns focus to the trigger, a click outside closes, ↑/↓ move.
  var menus = document.querySelectorAll("details.menu");
  function closeMenus(except) {
    menus.forEach(function (d) { if (d !== except) d.removeAttribute("open"); });
  }
  menus.forEach(function (menu) {
    menu.addEventListener("toggle", function () { if (menu.open) closeMenus(menu); });
    menu.addEventListener("keydown", function (event) {
      if (event.key === "Escape" && menu.open) {
        menu.removeAttribute("open");
        menu.querySelector("summary").focus();
        return;
      }
      if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
      var items = Array.prototype.slice.call(menu.querySelectorAll(".menu-pop a, .menu-pop button"));
      if (!items.length) return;
      if (!menu.open) menu.setAttribute("open", "");
      event.preventDefault();
      var i = items.indexOf(document.activeElement);
      i = event.key === "ArrowDown" ? (i + 1) % items.length : (i <= 0 ? items.length - 1 : i - 1);
      items[i].focus();
    });
  });
  document.addEventListener("click", function (event) {
    if (!event.target.closest || !event.target.closest("details.menu")) closeMenus(null);
  });

  // Selects that apply at once (<select data-autosubmit>); the Apply button
  // stays for pages without JavaScript (<noscript>).
  document.querySelectorAll("select[data-autosubmit]").forEach(function (select) {
    select.addEventListener("change", function () {
      if (select.form && select.form.requestSubmit) select.form.requestSubmit();
      else if (select.form) select.form.submit();
    });
  });

  // Flash messages can be dismissed.
  document.querySelectorAll("main > .flash").forEach(function (flash) {
    var close = document.createElement("button");
    close.type = "button";
    close.className = "dismiss";
    close.setAttribute("aria-label", "Dismiss");
    close.textContent = "×";
    close.addEventListener("click", function () { flash.remove(); });
    flash.appendChild(close);
  });

  // Tabs (<nav class="tabs">): ←/→ move between them.
  document.querySelectorAll(".tabs").forEach(function (tabs) {
    tabs.addEventListener("keydown", function (event) {
      if (event.key !== "ArrowRight" && event.key !== "ArrowLeft") return;
      var links = Array.prototype.slice.call(tabs.querySelectorAll("a"));
      var i = links.indexOf(document.activeElement);
      if (i < 0) return;
      event.preventDefault();
      links[(i + (event.key === "ArrowRight" ? 1 : links.length - 1)) % links.length].focus();
    });
  });

  // Copy buttons: <button data-copy="id">.
  document.querySelectorAll("[data-copy]").forEach(function (btn) {
    btn.addEventListener("click", function () {
      var source = document.getElementById(btn.getAttribute("data-copy"));
      if (source && navigator.clipboard) {
        navigator.clipboard.writeText(source.textContent.trim()).then(function () {
          var old = btn.textContent;
          btn.textContent = "Copied";
          setTimeout(function () { btn.textContent = old; }, 1500);
        });
      }
    });
  });
})();
