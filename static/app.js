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
