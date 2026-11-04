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

  // Toggle elements: <button data-toggle="id">.
  document.querySelectorAll("[data-toggle]").forEach(function (btn) {
    btn.addEventListener("click", function () {
      var target = document.getElementById(btn.getAttribute("data-toggle"));
      if (target) {
        target.classList.toggle("hidden");
        var focus = target.querySelector("input, textarea, select");
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
    var typeChosen = false;
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
