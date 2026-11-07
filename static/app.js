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
  //
  // On forms with data-chunked="<revision id>" (or "new-item" on the New item
  // form, whose sessions have no revision yet), files above CHUNKED_ABOVE
  // go first in chunks to /uploads (the sessions of /api/v1/uploads, see
  // src/web/uploads.rs); the form then lists them in its `uploads` field and
  // stores them with the other files in one transaction. A broken connection
  // costs one chunk: the script retries, and when it gives up, submitting the
  // same files again resumes where the server stopped (the session ids are
  // remembered per revision and file in localStorage).
  var CHUNKED_ABOVE = 32 * 1024 * 1024; // the desktop app's limit (desktop/src/api.rs)
  var NEW_ITEM = "new-item"; // data-chunked of the New item form
  var RETRIES = 6; // per chunk, waiting 1, 2, 4, … s

  function formatBytes(n) {
    if (n <= 0) return "0 KiB";
    if (n < 1024 * 1024) return Math.max(1, Math.round(n / 1024)) + " KiB";
    return (n / 1024 / 1024).toFixed(1) + " MiB";
  }

  function sessionKey(revision, file) {
    return "tc-upload:" + revision + ":" + file.name + ":" + file.size + ":" + file.lastModified;
  }
  function recall(key) {
    try { return window.localStorage.getItem(key); } catch (e) { return null; }
  }
  function remember(key, id) {
    try {
      if (id) window.localStorage.setItem(key, id); else window.localStorage.removeItem(key);
    } catch (e) { /* private mode: no resume after a reload */ }
  }

  // Goes to the redirect target of a script-sent form. A target on this very
  // page (`/revisions/…#files` after an upload there) would only change the
  // hash, so the page is reloaded to show the new files and the flash.
  function navigate(target) {
    var url;
    try { url = new URL(target, window.location.href); } catch (e) { window.location.assign(target); return; }
    if (url.pathname === window.location.pathname && url.search === window.location.search) {
      if (url.hash) window.location.hash = url.hash;
      window.location.reload();
    } else {
      window.location.assign(url.href);
    }
  }

  function jsonOf(xhr) {
    try { return JSON.parse(xhr.responseText); } catch (e) { return null; }
  }
  // The message of a JSON error answer ({"error": {"message": …}}).
  function reasonOf(xhr) {
    var body = jsonOf(xhr);
    if (body && body.error && body.error.message) return body.error.message;
    if (xhr.status === 401) return "Your session has ended – please sign in again.";
    if (xhr.status === 413) return "The upload exceeds the size limit of this server.";
    return "The server answered " + xhr.status + ".";
  }
  function failed(xhr) {
    var error = new Error(reasonOf(xhr));
    error.status = xhr.status;
    return error;
  }

  // One upload job: every request goes through job.send, so "Cancel upload"
  // can abort the one in flight.
  function newJob() {
    var job = { cancelled: false, xhr: null };
    job.send = function (method, url, body, headers, onProgress) {
      return new Promise(function (resolve, reject) {
        if (job.cancelled) { reject(cancelledError()); return; }
        var xhr = new XMLHttpRequest();
        xhr.open(method, url);
        xhr.setRequestHeader("X-TeamCraft-Script", "1");
        Object.keys(headers || {}).forEach(function (h) { xhr.setRequestHeader(h, headers[h]); });
        if (onProgress) {
          xhr.upload.addEventListener("progress", function (e) { onProgress(e.loaded, e.lengthComputable ? e.total : 0); });
        }
        xhr.addEventListener("load", function () {
          job.xhr = null;
          // an answer that arrives after "Cancel upload" counts as cancelled
          if (job.cancelled) reject(cancelledError()); else resolve(xhr);
        });
        xhr.addEventListener("error", function () {
          job.xhr = null;
          var error = new Error("The connection was interrupted.");
          error.network = true;
          reject(error);
        });
        xhr.addEventListener("abort", function () { job.xhr = null; reject(cancelledError()); });
        job.xhr = xhr;
        xhr.send(body === undefined ? null : body);
      });
    };
    job.wait = function (ms) {
      return new Promise(function (resolve, reject) {
        setTimeout(function () { if (job.cancelled) reject(cancelledError()); else resolve(); }, ms);
      });
    };
    job.cancel = function () {
      job.cancelled = true;
      if (job.xhr) job.xhr.abort();
    };
    return job;
  }
  function cancelledError() {
    var error = new Error("cancelled");
    error.cancelled = true;
    return error;
  }

  // Sends one large file in chunks; resolves with the id of the complete
  // upload session. onBytes(n) reports the bytes the server has (or is
  // receiving) of this file.
  function sendChunked(job, revision, file, onBytes, onSession) {
    var key = sessionKey(revision, file);
    var known = recall(key);

    function open() {
      var resumed = known
        ? job.send("GET", "/uploads/" + encodeURIComponent(known)).then(function (xhr) {
          var s = xhr.status === 200 ? jsonOf(xhr) : null;
          if (s && (s.revision_id || NEW_ITEM) === revision && s.size === file.size) return s;
          remember(key, null); // finished, cancelled or expired meanwhile
          return null;
        }, function (e) {
          if (e.cancelled) throw e;
          return null;
        })
        : Promise.resolve(null);
      return resumed.then(function (s) {
        if (s) return s;
        var body = JSON.stringify({
          revision_id: revision === NEW_ITEM ? null : revision,
          filename: file.name,
          size: file.size,
          content_type: file.type || null
        });
        return job.send("POST", "/uploads", body, { "Content-Type": "application/json" }).then(function (xhr) {
          var created = xhr.status === 201 ? jsonOf(xhr) : null;
          if (!created) throw failed(xhr);
          remember(key, created.id);
          return created;
        });
      });
    }

    return open().then(function (session) {
      onSession(session.id);
      var id = encodeURIComponent(session.id);
      var chunk = session.chunk_bytes > 0 ? session.chunk_bytes : 8 * 1024 * 1024;
      var offset = session.offset;
      var tries = 0;

      function next() {
        onBytes(offset);
        if (offset >= file.size) return Promise.resolve(session.id);
        var end = Math.min(file.size, offset + chunk);
        var headers = { "Upload-Offset": String(offset), "Content-Type": "application/offset+octet-stream" };
        var sentFrom = offset;
        return job.send("PATCH", "/uploads/" + id, file.slice(offset, end), headers, function (loaded) {
          onBytes(sentFrom + loaded);
        }).then(function (xhr) {
          var serverOffset = parseInt(xhr.getResponseHeader("Upload-Offset"), 10);
          if (xhr.status === 200) {
            offset = isNaN(serverOffset) ? end : serverOffset;
            tries = 0;
            return next();
          }
          // 409: the server has another offset (an earlier attempt arrived) – continue there.
          if (xhr.status === 409 && !isNaN(serverOffset) && serverOffset !== sentFrom) {
            offset = serverOffset;
            return next();
          }
          if (xhr.status >= 500) return retry(failed(xhr));
          throw failed(xhr);
        }, function (e) {
          if (e.cancelled) throw e;
          return retry(e);
        });
      }

      // Waits, asks the server where to continue and goes on.
      function retry(cause) {
        tries += 1;
        if (tries > RETRIES) throw cause;
        return job.wait(1000 * Math.pow(2, tries - 1)).then(function () {
          return job.send("GET", "/uploads/" + id);
        }).then(function (xhr) {
          var s = xhr.status === 200 ? jsonOf(xhr) : null;
          if (!s) {
            if (xhr.status >= 500) return retry(failed(xhr));
            remember(key, null);
            throw failed(xhr);
          }
          offset = s.offset;
          return next();
        }, function (e) {
          if (e.cancelled) throw e;
          return retry(e.network ? e : cause);
        });
      }

      return next();
    });
  }

  document.querySelectorAll("form[enctype='multipart/form-data']").forEach(function (form) {
    form.addEventListener("submit", function (event) {
      if (event.defaultPrevented || !window.FormData || !window.XMLHttpRequest) return;
      var inputs = Array.prototype.filter.call(form.querySelectorAll("input[type=file]"), function (input) {
        return input.files && input.files.length > 0;
      });
      if (inputs.length === 0) return; // nothing to transfer: a normal submit is instant
      event.preventDefault();

      // Which files go in chunks (only where the form supports it).
      var revision = form.getAttribute("data-chunked");
      var canChunk = !!(revision && window.Promise && window.Blob && Blob.prototype.slice &&
        FormData.prototype.delete);
      var large = [];
      var small = [];
      inputs.forEach(function (input) {
        Array.prototype.forEach.call(input.files, function (file) {
          if (canChunk && input.name === "file" && file.size > CHUNKED_ABOVE) large.push(file);
          else small.push({ name: input.name, file: file });
        });
      });
      var data = new FormData(form);
      if (large.length > 0) {
        // The large files leave the form; it lists their upload sessions instead.
        inputs.forEach(function (input) { data.delete(input.name); });
        small.forEach(function (entry) { data.append(entry.name, entry.file, entry.file.name); });
      }
      var largeBytes = large.reduce(function (sum, f) { return sum + f.size; }, 0);
      var smallBytes = small.reduce(function (sum, e) { return sum + e.file.size; }, 0);
      var totalBytes = largeBytes + smallBytes;

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
          setTimeout(function () { note.remove(); }, isError ? 15000 : 6000);
        }
      }
      function show(sent, name) {
        var percent = totalBytes > 0 ? Math.min(100, Math.floor((sent / totalBytes) * 100)) : 100;
        bar.value = percent;
        label.textContent = "Uploading " + (name ? name + " · " : "") + percent + " % · " +
          formatBytes(sent) + " of " + formatBytes(totalBytes);
      }

      var job = newJob();
      var sessions = []; // ids of this form's chunked uploads
      cancel.addEventListener("click", function () {
        job.cancel();
        // Nothing stays behind on the server: the sessions are removed.
        sessions.forEach(function (id) {
          var xhr = new XMLHttpRequest();
          xhr.open("DELETE", "/uploads/" + encodeURIComponent(id));
          xhr.setRequestHeader("X-TeamCraft-Script", "1");
          xhr.send();
        });
        large.forEach(function (file) { remember(sessionKey(revision, file), null); });
      });

      // 1. the large files, one after the other
      var sentBefore = 0;
      var chain = Promise.resolve();
      large.forEach(function (file) {
        chain = chain.then(function () {
          return sendChunked(job, revision, file, function (n) { show(sentBefore + n, file.name); }, function (id) {
            if (sessions.indexOf(id) < 0) sessions.push(id);
          }).then(function () { sentBefore += file.size; });
        });
      });

      // 2. the form itself with the small files and the upload ids
      chain.then(function () {
        if (sessions.length > 0) data.append("uploads", JSON.stringify(sessions));
        return job.send("POST", form.action, data, null, function (loaded, total) {
          show(largeBytes + (total > 0 ? (loaded / total) * smallBytes : 0));
          if (total > 0 && loaded >= total) {
            // everything is sent: the server stores the files in one
            // transaction, which cannot be cancelled half-way any more
            bar.removeAttribute("value");
            label.textContent = "Storing the files …";
            cancel.remove();
          }
        });
      }).then(function (xhr) {
        var target = (jsonOf(xhr) || {}).location;
        if (xhr.status === 200 && target) {
          navigate(target);
        } else {
          done("The upload failed: " + reasonOf(xhr) + " Please try again.", true);
        }
      }, function (e) {
        if (e.cancelled) {
          done("Upload cancelled – nothing was changed.", false);
        } else if (large.length > 0 && e.status !== 401) {
          done("The upload stopped (" + e.message + ") – nothing was changed yet. Submit the form again " +
            "with the same files to continue where it stopped.", true);
        } else if (e.network) {
          done("The upload failed – check the connection and try again.", true);
        } else {
          done("The upload failed: " + e.message, true);
        }
      });
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
  // <time data-date>: the date alone, for narrow columns (the title keeps the time).
  var dateFmt = null;
  try { dateFmt = new Intl.DateTimeFormat(undefined, { year: "numeric", month: "2-digit", day: "2-digit" }); } catch (e) { dateFmt = null; }
  if (fmt) {
    document.querySelectorAll("time[datetime]").forEach(function (el) {
      var d = new Date(el.getAttribute("datetime"));
      if (!isNaN(d.getTime())) {
        el.title = fmt.format(d);
        el.textContent = el.hasAttribute("data-date") && dateFmt ? dateFmt.format(d) : fmt.format(d);
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

  // Toggle elements: <button data-toggle="id">. Esc inside an opened panel
  // closes it again and returns focus to the button that opened it.
  var openedBy = {};
  function setExpanded(id, open) {
    document.querySelectorAll('[data-toggle="' + id + '"][aria-expanded]').forEach(function (b) {
      b.setAttribute("aria-expanded", String(open));
    });
  }
  document.querySelectorAll("[data-toggle]").forEach(function (btn) {
    btn.addEventListener("click", function () {
      var id = btn.getAttribute("data-toggle");
      var target = document.getElementById(id);
      if (target) {
        target.classList.toggle("hidden");
        var open = !target.classList.contains("hidden");
        setExpanded(id, open);
        if (open) openedBy[id] = btn;
        var focus = target.querySelector("input:not([type=hidden]), textarea, select");
        if (focus && open) focus.focus();
      }
    });
  });
  document.addEventListener("keydown", function (event) {
    if (event.key !== "Escape" || event.defaultPrevented) return;
    var el = event.target;
    while (el && el !== document.body) {
      var opener = el.id && openedBy[el.id];
      if (opener && !el.classList.contains("hidden")) {
        el.classList.add("hidden");
        setExpanded(el.id, false);
        delete openedBy[el.id];
        opener.focus();
        event.preventDefault();
        return;
      }
      el = el.parentElement;
    }
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
  // user is typing somewhere; Ctrl+K / ⌘K does the same. The single-key "/"
  // can be switched off on the account page (WCAG 2.1.4), per browser.
  function slashShortcutOn() {
    try { return window.localStorage.getItem("tc-slash-shortcut") !== "off"; } catch (e) { return true; }
  }
  if (!slashShortcutOn()) document.documentElement.setAttribute("data-slash", "off");
  document.addEventListener("keydown", function (event) {
    var slash = event.key === "/" && !event.ctrlKey && !event.metaKey && !event.altKey;
    var ctrlK = (event.key === "k" || event.key === "K") && (event.ctrlKey || event.metaKey) && !event.altKey;
    if (!slash && !ctrlK) return;
    if (slash && !slashShortcutOn()) return;
    var t = event.target;
    if (slash && t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))) return;
    var field = document.querySelector("[data-filter]") || document.querySelector(".quicksearch input");
    if (field) { event.preventDefault(); field.focus(); field.select(); }
  });
  document.querySelectorAll("input[data-slash-pref]").forEach(function (box) {
    var wrap = box.closest(".hidden");
    if (wrap) wrap.classList.remove("hidden");
    box.checked = slashShortcutOn();
    box.addEventListener("change", function () {
      try { window.localStorage.setItem("tc-slash-shortcut", box.checked ? "on" : "off"); } catch (e) { /* storage blocked */ }
      if (box.checked) document.documentElement.removeAttribute("data-slash");
      else document.documentElement.setAttribute("data-slash", "off");
    });
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
        event.preventDefault(); // an enclosing toggled panel stays open
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
  // stays for pages without JavaScript (<noscript>). Only a choice made with
  // the pointer submits at once: ↑/↓ on a closed select fire "change" on
  // every step (WCAG 3.2.2), so the keyboard applies with Enter or on leaving.
  document.querySelectorAll("select[data-autosubmit]").forEach(function (select) {
    var initial = select.value, pointer = false, sent = false;
    function apply() {
      if (sent || select.value === initial || !select.form) return;
      sent = true;
      if (select.form.requestSubmit) select.form.requestSubmit();
      else select.form.submit();
    }
    select.addEventListener("pointerdown", function () { pointer = true; });
    select.addEventListener("keydown", function (event) {
      pointer = false;
      if (event.key === "Enter") { event.preventDefault(); apply(); }
    });
    select.addEventListener("change", function () { if (pointer) apply(); });
    select.addEventListener("blur", apply);
  });

  // BOM tree: every line with lines below it gets a toggle button that folds
  // its branch. Without JavaScript all rows stay visible. Rows carry
  // data-level; the lines below a row are the following rows with a higher
  // level.
  document.querySelectorAll("table[data-bom-tree]").forEach(function (table) {
    var rows = Array.prototype.slice.call(table.tBodies[0].rows);
    var levelOf = function (row) { return parseInt(row.getAttribute("data-level"), 10) || 1; };
    var folded = new Set();
    var buttons = new Map();

    // Hides every row below a folded row (also below an open row inside it).
    function apply() {
      var hideBelow = null;
      rows.forEach(function (row) {
        var level = levelOf(row);
        if (hideBelow !== null && level > hideBelow) { row.hidden = true; return; }
        hideBelow = null;
        row.hidden = false;
        if (folded.has(row)) hideBelow = level;
      });
      buttons.forEach(function (button, row) {
        var open = !folded.has(row);
        button.setAttribute("aria-expanded", String(open));
        button.firstChild.textContent = open ? "▾" : "▸";
      });
    }

    rows.forEach(function (row, i) {
      var next = rows[i + 1];
      var cell = row.querySelector("td.bom-item");
      if (!next || !cell || levelOf(next) <= levelOf(row)) return;
      var button = document.createElement("button");
      var glyph = document.createElement("span");
      glyph.setAttribute("aria-hidden", "true");
      button.appendChild(glyph);
      button.type = "button";
      button.className = "tree-toggle";
      var name = row.getAttribute("data-tc-row") || "line";
      button.setAttribute("aria-label", "Fold or unfold the lines below " + name);
      button.addEventListener("click", function () {
        if (folded.has(row)) folded["delete"](row); else folded.add(row);
        apply();
      });
      cell.insertBefore(button, cell.firstChild);
      buttons.set(row, button);
    });
    if (!buttons.size) return;
    apply();

    var tools = document.querySelector("[data-bom-tools]");
    if (tools) {
      tools.hidden = false;
      tools.querySelector("[data-bom-collapse-all]").addEventListener("click", function () {
        // every branch folds; unfolding one keeps its folded sub-branches folded
        buttons.forEach(function (button, row) { folded.add(row); });
        apply();
      });
      tools.querySelector("[data-bom-expand-all]").addEventListener("click", function () {
        folded.clear();
        apply();
      });
    }
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

  // Assistant sidebar ("Ask TeamCraft"): the shared module
  // (shared/js/assistant.js) is loaded when the drawer is first opened. The
  // page declares where the user is with data-tc-* attributes on <main>; the
  // server resolves those identifiers with the user's rights, the markup is
  // only a hint.
  (function () {
    var host = document.getElementById("assistant-drawer");
    var toggle = document.getElementById("assistant-toggle");
    var modulePath = host && host.getAttribute("data-module");
    if (!host || !toggle || !modulePath) return;
    var drawer = null, pending = null;

    var FIELDS = { kind: "tcKind", item: "tcItem", revision: "tcRevision", change: "tcChange", request: "tcRequest",
      project: "tcProject", view: "tcView", rule: "tcRule", query: "tcQuery", selected: "tcSelected" };

    function pageContext() {
      var main = document.getElementById("main");
      var ctx = {};
      if (!main) return ctx;
      Object.keys(FIELDS).forEach(function (key) {
        var value = main.dataset[FIELDS[key]];
        if (value) ctx[key] = value;
      });
      var filters = {}, any = false;
      Object.keys(main.dataset).forEach(function (name) {
        var m = /^tcFilter(.+)$/.exec(name);
        if (m && main.dataset[name]) { filters[m[1].toLowerCase()] = main.dataset[name]; any = true; }
      });
      if (any) ctx.filters = filters;
      var shown = [];
      main.querySelectorAll("[data-tc-row]").forEach(function (row) {
        var id = row.getAttribute("data-tc-row");
        if (id && shown.length < 25 && shown.indexOf(id) === -1) shown.push(id);
      });
      if (shown.length) ctx.shown = shown;
      return ctx;
    }

    function request(method, path, body, signal) {
      return fetch(path, {
        method: method,
        credentials: "same-origin",
        headers: { "Accept": "application/json", "Content-Type": "application/json" },
        body: body ? JSON.stringify(body) : undefined,
        signal: signal
      }).then(function (response) {
        return response.json().then(function (json) { return { ok: response.ok, json: json }; }, function () {
          return { ok: false, json: { error: "The server did not answer as expected – reload the page (your session may have ended)." } };
        });
      }).then(function (r) {
        if (!r.ok) {
          var e = r.json && r.json.error;
          throw new Error((e && (e.message || (typeof e === "string" ? e : ""))) || "The assistant could not answer.");
        }
        return r.json;
      });
    }

    function ensure() {
      if (drawer) return Promise.resolve(drawer);
      if (!pending) {
        pending = import(modulePath).then(function (mod) {
          drawer = mod.mount(host, {
            opener: toggle,
            canEdit: host.getAttribute("data-can-edit") === "1",
            privacy: host.getAttribute("data-privacy") || "the model provider",
            getContext: pageContext,
            link: function (number) {
              var a = document.createElement("a");
              a.href = "/i/" + encodeURIComponent(number);
              a.textContent = number;
              return a;
            },
            api: {
              thread: function () { return request("GET", "/assistant/thread"); },
              send: function (body, signal) { return request("POST", "/assistant/chat", body, signal); },
              reset: function () { return request("POST", "/assistant/thread/reset", {}); }
            },
            onToggle: function (open) { toggle.setAttribute("aria-expanded", String(open)); }
          });
          return drawer;
        }, function (error) { pending = null; throw error; });
      }
      return pending;
    }

    toggle.addEventListener("click", function () {
      ensure().then(function (d) { d.toggle(); });
    });
    document.querySelectorAll("[data-assistant-open]").forEach(function (button) {
      button.addEventListener("click", function () { ensure().then(function (d) { d.open(); }); });
    });
    var wasOpen = false;
    try { wasOpen = window.localStorage.getItem("tc.assistant.open") === "1"; } catch (e) { /* storage blocked */ }
    if (wasOpen && window.matchMedia("(min-width: 641px)").matches) {
      ensure().then(function (d) { d.open(false); });
    }
  })();
})();
