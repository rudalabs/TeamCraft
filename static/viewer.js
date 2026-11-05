// TeamCraft 3D preview: a small self-contained WebGL2 viewer.
//
// The server converts every supported file (meshes, point clouds, STEP) into
// one compact binary mesh ("TCMESH01", see src/mesh.rs); this viewer only
// draws it – nothing is parsed or tessellated in the browser. Simulation
// results carry scalar fields; the viewer colours them with a colour map and
// shows a legend (with a field selector) right above the canvas.
//
// Web pages mount the viewer through data attributes on #viewer-canvas; the
// desktop app calls TeamCraftViewer.mount(canvas, options) with its own loader.
//
// Mouse: drag = rotate, wheel = zoom, double-click = reset. Touch: one finger
// rotates, pinch zooms. Keyboard (canvas focused): arrows rotate, +/- zoom,
// 0 resets.
(function () {
  "use strict";

  /**
   * Shows a model on `canvas`.
   *
   * options.load      () => Promise<ArrayBuffer> – the TCMESH01 mesh
   * options.statusEl, options.resetBtn, options.upBtn – optional elements
   * options.onReady   called when the model is shown
   *
   * Returns { exportStl() → ArrayBuffer (binary STL), dispose() }.
   */
  function mount(canvas, options) {
    options = options || {};
    var statusEl = options.statusEl || null;
    var resetBtn = options.resetBtn || null;
    var upBtn = options.upBtn || null;
    var disposed = false;
    var windowListeners = [];

    function setStatus(text, isError) {
      if (!statusEl || disposed) return;
      statusEl.textContent = text;
      statusEl.className = isError ? "flash flash-error" : "small muted";
    }

    // ---------------------------------------------------------------- mesh

    function parseMesh(buffer) {
      var magic = new TextDecoder().decode(new Uint8Array(buffer, 0, Math.min(8, buffer.byteLength)));
      if (buffer.byteLength < 48 || magic !== "TCMESH01") throw new Error("The server sent no 3D model.");
      var dv = new DataView(buffer);
      var vertices = dv.getUint32(8, true), indices = dv.getUint32(12, true);
      var parts = dv.getUint32(16, true), flags = dv.getUint32(20, true);
      var bounds = [];
      for (var i = 0; i < 6; i++) bounds.push(dv.getFloat32(24 + i * 4, true));
      var at = 48;
      var positions = new Float32Array(buffer, at, vertices * 3);
      at += vertices * 12;
      var index = indices ? new Uint32Array(buffer, at, indices) : null;
      at += indices * 4;
      var colors = flags & 1 ? new Uint8Array(buffer, at, vertices * 3) : null;
      if (colors) at += vertices * 3;
      // flag 2: scalar result fields (simulation results), see Mesh::encode
      var fields = [];
      if (flags & 2) {
        at = Math.ceil(at / 4) * 4;
        var text = function () {
          var n = dv.getUint32(at, true);
          var s = new TextDecoder().decode(new Uint8Array(buffer, at + 4, n));
          at += 4 + n;
          return s;
        };
        var count = dv.getUint32(at, true);
        at += 4;
        for (var f = 0; f < count && f < 8; f++) {
          var name = text(), unit = text();
          at = Math.ceil(at / 4) * 4;
          var min = dv.getFloat32(at, true), max = dv.getFloat32(at + 4, true);
          at += 8;
          fields.push({ name: name, unit: unit, min: min, max: max, values: new Float32Array(buffer, at, vertices) });
          at += vertices * 4;
        }
      }
      // flag 4: partial preview, face counts in the last 8 bytes (after the fields)
      var faces = flags & 4 && buffer.byteLength >= 56
        ? { shown: dv.getUint32(buffer.byteLength - 8, true), total: dv.getUint32(buffer.byteLength - 4, true) }
        : null;
      return { positions: positions, indices: index, colors: colors, parts: parts, bounds: bounds, fields: fields, faces: faces };
    }

    // ---------------------------------------------------------------- colour map

    // Perceptually uniform (after matplotlib's "viridis", CC0); the same
    // stops as COLOR_MAP in src/mesh.rs. 256-entry lookup table.
    var STOPS = [[68, 1, 84], [71, 45, 123], [59, 82, 139], [44, 114, 142], [33, 145, 140],
      [40, 174, 128], [94, 201, 98], [173, 220, 48], [253, 231, 37]];
    var LUT = new Uint8Array(256 * 3);
    for (var li = 0; li < 256; li++) {
      var x = li / 255 * (STOPS.length - 1), si = Math.min(Math.floor(x), STOPS.length - 2), sf = x - si;
      for (var lc = 0; lc < 3; lc++) LUT[li * 3 + lc] = Math.round(STOPS[si][lc] + (STOPS[si + 1][lc] - STOPS[si][lc]) * sf);
    }
    var NO_VALUE = [176, 188, 200];

    function fieldColors(field, out) {
      var v = field.values, lo = field.min, span = field.max > field.min ? field.max - field.min : 1;
      for (var i = 0; i < v.length; i++) {
        var o = i * 3, val = v[i];
        if (val !== val || val === Infinity || val === -Infinity) {
          out[o] = NO_VALUE[0]; out[o + 1] = NO_VALUE[1]; out[o + 2] = NO_VALUE[2];
          continue;
        }
        var k = Math.max(0, Math.min(255, Math.round((val - lo) / span * 255))) * 3;
        out[o] = LUT[k]; out[o + 1] = LUT[k + 1]; out[o + 2] = LUT[k + 2];
      }
      return out;
    }

    function formatValue(v) {
      if (v === 0) return "0";
      var a = Math.abs(v);
      if (a >= 1e5 || a < 1e-3) return v.toExponential(3);
      return Number(v.toPrecision(4)).toLocaleString();
    }

    // Legend above the canvas: field (selector when there are several), min, bar, max.
    function legend(mesh, onSelect) {
      var el = document.createElement("div");
      el.className = "viewer-legend";
      // a named group: a plain div's aria-label is not announced
      el.setAttribute("role", "group");
      var label;
      if (mesh.fields.length > 1) {
        label = document.createElement("select");
        label.className = "small";
        label.setAttribute("aria-label", "Result field");
        mesh.fields.forEach(function (f, i) {
          var opt = document.createElement("option");
          opt.value = String(i);
          opt.textContent = f.name + (f.unit ? " [" + f.unit + "]" : "");
          label.appendChild(opt);
        });
        label.addEventListener("change", function () { onSelect(Number(label.value)); });
      } else {
        label = document.createElement("strong");
        label.className = "viewer-legend-name";
      }
      var lo = document.createElement("span"), hi = document.createElement("span");
      lo.className = hi.className = "viewer-legend-value";
      var bar = document.createElement("canvas");
      bar.className = "viewer-legend-bar";
      bar.width = 256;
      bar.height = 1;
      bar.setAttribute("aria-hidden", "true");
      var ctx = bar.getContext("2d");
      if (ctx) {
        var img = ctx.createImageData(256, 1);
        for (var i = 0; i < 256; i++) {
          img.data[i * 4] = LUT[i * 3]; img.data[i * 4 + 1] = LUT[i * 3 + 1];
          img.data[i * 4 + 2] = LUT[i * 3 + 2]; img.data[i * 4 + 3] = 255;
        }
        ctx.putImageData(img, 0, 0);
      }
      el.appendChild(label);
      el.appendChild(lo);
      el.appendChild(bar);
      el.appendChild(hi);
      function show(f) {
        if (label.tagName === "STRONG") label.textContent = f.name;
        var unit = f.unit ? " " + f.unit : "";
        lo.textContent = formatValue(f.min) + unit;
        hi.textContent = formatValue(f.max) + unit;
        el.setAttribute("aria-label", f.name + ": colour scale from " + lo.textContent + " to " + hi.textContent);
      }
      return { el: el, show: show };
    }

    // ---------------------------------------------------------------- math

    function multiply(a, b) {
      var out = new Float32Array(16);
      for (var c = 0; c < 4; c++) {
        for (var r = 0; r < 4; r++) {
          var s = 0;
          for (var k = 0; k < 4; k++) s += a[k * 4 + r] * b[c * 4 + k];
          out[c * 4 + r] = s;
        }
      }
      return out;
    }

    function perspective(fovy, aspect, near, far) {
      var f = 1 / Math.tan(fovy / 2), nf = 1 / (near - far);
      return new Float32Array([f / aspect, 0, 0, 0, 0, f, 0, 0, 0, 0, (far + near) * nf, -1, 0, 0, 2 * far * near * nf, 0]);
    }

    function translation(x, y, z) {
      return new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, x, y, z, 1]);
    }

    function rotationX(a) {
      var c = Math.cos(a), s = Math.sin(a);
      return new Float32Array([1, 0, 0, 0, 0, c, s, 0, 0, -s, c, 0, 0, 0, 0, 1]);
    }

    function rotationY(a) {
      var c = Math.cos(a), s = Math.sin(a);
      return new Float32Array([c, 0, -s, 0, 0, 1, 0, 0, s, 0, c, 0, 0, 0, 0, 1]);
    }

    // ---------------------------------------------------------------- colours

    function cssColor(name, fallback) {
      // From the canvas: an element with its own data-skin paints that skin.
      var raw = getComputedStyle(canvas).getPropertyValue(name).trim();
      var m = /^#([0-9a-f]{6})$/i.exec(raw);
      if (!m) return fallback;
      var v = parseInt(m[1], 16);
      return [((v >> 16) & 255) / 255, ((v >> 8) & 255) / 255, (v & 255) / 255];
    }

    // ---------------------------------------------------------------- renderer

    var gl = canvas.getContext("webgl2", { antialias: true });
    if (!gl) {
      setStatus("Your browser does not support WebGL 2 – download the file to view it.", true);
      return { exportStl: function () { throw new Error("No WebGL 2."); }, dispose: function () {} };
    }

    // Flat shading from screen-space derivatives: no normals have to be
    // transferred, and every face is lit from the viewer's side.
    var VERT = "#version 300 es\nin vec3 aPos; in vec3 aCol; uniform mat4 uMvp; uniform mat4 uMv; uniform float uSize;\n" +
      "out vec3 vView; out vec3 vCol;\n" +
      "void main() { vCol = aCol; vView = (uMv * vec4(aPos, 1.0)).xyz; gl_Position = uMvp * vec4(aPos, 1.0); gl_PointSize = uSize; }";
    var FRAG = "#version 300 es\nprecision highp float; in vec3 vView; in vec3 vCol; uniform bool uPoints; out vec4 frag;\n" +
      "void main() { if (uPoints) { frag = vec4(vCol, 1.0); return; }\n" +
      "  vec3 n = normalize(cross(dFdx(vView), dFdy(vView)));\n" +
      "  float d = max(dot(n, normalize(vec3(0.35, 0.55, 1.0))), 0.0);\n" +
      "  float rim = pow(1.0 - max(n.z, 0.0), 3.0) * 0.18;\n" +
      "  frag = vec4(vCol * (0.25 + 0.75 * d) + rim, 1.0); }";

    function shader(type, source) {
      var s = gl.createShader(type);
      gl.shaderSource(s, source);
      gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s));
      return s;
    }

    var program = gl.createProgram();
    gl.attachShader(program, shader(gl.VERTEX_SHADER, VERT));
    gl.attachShader(program, shader(gl.FRAGMENT_SHADER, FRAG));
    gl.linkProgram(program);
    var loc = {
      pos: gl.getAttribLocation(program, "aPos"),
      col: gl.getAttribLocation(program, "aCol"),
      mvp: gl.getUniformLocation(program, "uMvp"),
      mv: gl.getUniformLocation(program, "uMv"),
      size: gl.getUniformLocation(program, "uSize"),
      points: gl.getUniformLocation(program, "uPoints")
    };

    var drawable = null;
    // zUp: CAD convention (NX, CATIA, FreeCAD, 3D printing); toggle for Y-up exports.
    var view = { yaw: -0.6, pitch: 0.45, dist: 1, center: [0, 0, 0], radius: 1, zUp: true };
    var home = null;
    // Model and canvas colours from the skin tokens (skins.css); read again
    // when the system switches between light and dark (the System skin).
    var baseColor, clearColor;
    function readColors() {
      baseColor = cssColor("--viewer-model", cssColor("--c3", [0.435, 0.529, 0.631]));
      clearColor = cssColor("--viewer-bg", cssColor("--bg-raised", [0.925, 0.937, 0.953]));
    }
    readColors();
    var scheme = window.matchMedia ? window.matchMedia("(prefers-color-scheme: dark)") : null;
    function onScheme() { readColors(); redraw(); }
    if (scheme && scheme.addEventListener) scheme.addEventListener("change", onScheme);

    function upload(mesh) {
      var vao = gl.createVertexArray();
      gl.bindVertexArray(vao);
      var pb = gl.createBuffer();
      gl.bindBuffer(gl.ARRAY_BUFFER, pb);
      gl.bufferData(gl.ARRAY_BUFFER, mesh.positions, gl.STATIC_DRAW);
      gl.enableVertexAttribArray(loc.pos);
      gl.vertexAttribPointer(loc.pos, 3, gl.FLOAT, false, 0, 0);
      var cb = null;
      // result fields are coloured here (the baked colours are for old viewers)
      var colors = mesh.fields.length ? fieldColors(mesh.fields[0], new Uint8Array(mesh.positions.length)) : mesh.colors;
      if (colors) {
        cb = gl.createBuffer();
        gl.bindBuffer(gl.ARRAY_BUFFER, cb);
        gl.bufferData(gl.ARRAY_BUFFER, colors, mesh.fields.length ? gl.DYNAMIC_DRAW : gl.STATIC_DRAW);
        gl.enableVertexAttribArray(loc.col);
        gl.vertexAttribPointer(loc.col, 3, gl.UNSIGNED_BYTE, true, 0, 0);
      } else {
        gl.disableVertexAttribArray(loc.col);
      }
      var count = mesh.positions.length / 3;
      if (mesh.indices) {
        var ib = gl.createBuffer();
        gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, ib);
        gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, mesh.indices, gl.STATIC_DRAW);
        count = mesh.indices.length;
      }
      gl.bindVertexArray(null);
      return { vao: vao, count: count, points: !mesh.indices, colored: !!colors, colorBuffer: cb, colors: colors };
    }

    // switches the shown result field (no animation: it is a frequent action)
    function selectField(i) {
      var f = loaded && loaded.fields[i];
      if (!f || !drawable || !drawable.colorBuffer) return;
      fieldColors(f, drawable.colors);
      gl.bindBuffer(gl.ARRAY_BUFFER, drawable.colorBuffer);
      gl.bufferSubData(gl.ARRAY_BUFFER, 0, drawable.colors);
      if (legendView) legendView.show(f);
      redraw();
    }
    var legendView = null;

    function frame(mesh) {
      var b = mesh.bounds;
      var size = [b[3] - b[0], b[4] - b[1], b[5] - b[2]];
      view.center = [(b[0] + b[3]) / 2, (b[1] + b[4]) / 2, (b[2] + b[5]) / 2];
      view.radius = Math.max(Math.hypot(size[0], size[1], size[2]) / 2, 1e-6);
      view.dist = view.radius * 2.8;
      home = { yaw: view.yaw, pitch: view.pitch, dist: view.dist };
      function fmt(v) { return (Math.round(v * 100) / 100).toLocaleString(); }
      var what = mesh.indices
        ? (mesh.indices.length / 3).toLocaleString() + " triangles · " + mesh.parts.toLocaleString() + (mesh.parts === 1 ? " body" : " bodies")
        : (mesh.positions.length / 3).toLocaleString() + " points";
      var partial = mesh.faces
        ? " · partial preview: " + mesh.faces.shown.toLocaleString() + " of " + mesh.faces.total.toLocaleString() + " faces"
        : "";
      setStatus(what + " · " + fmt(size[0]) + " × " + fmt(size[1]) + " × " + fmt(size[2]) + " (X × Y × Z, model units)" + partial);
    }

    function render() {
      var dpr = window.devicePixelRatio || 1;
      var w = Math.max(1, Math.round(canvas.clientWidth * dpr));
      var h = Math.max(1, Math.round(canvas.clientHeight * dpr));
      if (canvas.width !== w || canvas.height !== h) { canvas.width = w; canvas.height = h; }
      gl.viewport(0, 0, w, h);
      gl.clearColor(clearColor[0], clearColor[1], clearColor[2], 1);
      gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);
      gl.enable(gl.DEPTH_TEST);
      if (!drawable) return;
      var rot = multiply(rotationX(view.pitch), rotationY(view.yaw));
      if (view.zUp) rot = multiply(rot, rotationX(-Math.PI / 2));
      var c = view.center;
      var model = multiply(rot, translation(-c[0], -c[1], -c[2]));
      var viewM = multiply(translation(0, 0, -view.dist), model);
      var proj = perspective(Math.PI / 4, w / h, Math.max(view.dist - view.radius * 4, view.radius / 100), view.dist + view.radius * 4);
      gl.useProgram(program);
      gl.uniformMatrix4fv(loc.mvp, false, multiply(proj, viewM));
      gl.uniformMatrix4fv(loc.mv, false, viewM);
      gl.uniform1f(loc.size, Math.max(1, Math.round(1.5 * dpr)));
      gl.uniform1i(loc.points, drawable.points ? 1 : 0);
      gl.bindVertexArray(drawable.vao);
      if (!drawable.colored) gl.vertexAttrib3f(loc.col, baseColor[0], baseColor[1], baseColor[2]);
      if (drawable.points) gl.drawArrays(gl.POINTS, 0, drawable.count);
      else gl.drawElements(gl.TRIANGLES, drawable.count, gl.UNSIGNED_INT, 0);
      gl.bindVertexArray(null);
    }

    var pending = false;
    function redraw() {
      if (pending || disposed) return;
      pending = true;
      requestAnimationFrame(function () { pending = false; if (!disposed) render(); });
    }

    function zoom(factor) {
      view.dist = Math.max(view.radius * 0.05, Math.min(view.radius * 200, view.dist * factor));
      redraw();
    }

    function rotate(dYaw, dPitch) {
      view.yaw += dYaw;
      view.pitch = Math.max(-1.55, Math.min(1.55, view.pitch + dPitch));
      redraw();
    }

    function reset() {
      if (!home) return;
      view.yaw = home.yaw; view.pitch = home.pitch; view.dist = home.dist;
      redraw();
    }

    // ---------------------------------------------------------------- input

    var pointers = {};
    var pinch = null;
    canvas.addEventListener("pointerdown", function (e) {
      canvas.setPointerCapture(e.pointerId);
      pointers[e.pointerId] = { x: e.clientX, y: e.clientY };
    });
    canvas.addEventListener("pointermove", function (e) {
      var prev = pointers[e.pointerId];
      if (!prev) return;
      var ids = Object.keys(pointers);
      if (ids.length === 2) {
        var other = pointers[ids[0] == e.pointerId ? ids[1] : ids[0]];
        var d = Math.hypot(e.clientX - other.x, e.clientY - other.y);
        if (pinch) zoom(pinch / Math.max(d, 1));
        pinch = d;
      } else {
        rotate((e.clientX - prev.x) * 0.01, (e.clientY - prev.y) * 0.01);
      }
      pointers[e.pointerId] = { x: e.clientX, y: e.clientY };
    });
    function release(e) { delete pointers[e.pointerId]; pinch = null; }
    canvas.addEventListener("pointerup", release);
    canvas.addEventListener("pointercancel", release);
    canvas.addEventListener("wheel", function (e) {
      if (e.ctrlKey) return; // Ctrl + wheel zooms the page, not the model
      e.preventDefault();
      zoom(Math.exp(e.deltaY * 0.0012));
    }, { passive: false });
    canvas.addEventListener("dblclick", reset);
    canvas.addEventListener("keydown", function (e) {
      if (e.ctrlKey || e.metaKey || e.altKey) return; // page shortcuts (zoom, …)
      var step = 0.12;
      switch (e.key) {
        case "ArrowLeft": rotate(-step, 0); break;
        case "ArrowRight": rotate(step, 0); break;
        case "ArrowUp": rotate(0, -step); break;
        case "ArrowDown": rotate(0, step); break;
        case "+": case "=": zoom(0.85); break;
        case "-": case "_": zoom(1 / 0.85); break;
        case "0": reset(); break;
        default: return;
      }
      e.preventDefault();
    });
    if (resetBtn) resetBtn.addEventListener("click", reset);
    if (upBtn) {
      upBtn.addEventListener("click", function () {
        view.zUp = !view.zUp;
        upBtn.textContent = view.zUp ? "Z up" : "Y up";
        redraw();
      });
    }
    window.addEventListener("resize", redraw);
    windowListeners.push(["resize", redraw]);

    // ---------------------------------------------------------------- load

    setStatus("Loading the model …");
    render();
    var loaded = null;
    Promise.resolve()
      .then(function () { return options.load(); })
      .then(function (buffer) {
        if (disposed) return;
        var mesh = parseMesh(buffer);
        frame(mesh);
        loaded = mesh;
        drawable = upload(mesh);
        if (mesh.fields.length) {
          legendView = legend(mesh, selectField);
          legendView.show(mesh.fields[0]);
          canvas.insertAdjacentElement("beforebegin", legendView.el);
        }
        canvas.setAttribute("data-ready", "true");
        if (options.onReady) options.onReady();
        redraw();
      })
      .catch(function (err) {
        if (disposed) return;
        canvas.setAttribute("data-ready", "error");
        setStatus((err && err.message) || String(err), true);
      });

    // Binary STL of the shown triangles, in model coordinates as in the file
    // (STL has no units: STEP exports are usually millimetres).
    function exportStl() {
      if (!loaded || !loaded.indices) throw new Error("There are no triangles to export.");
      var p = loaded.positions, idx = loaded.indices, count = idx.length / 3;
      var buffer = new ArrayBuffer(84 + count * 50), dv = new DataView(buffer);
      var header = "TeamCraft STL export";
      for (var h = 0; h < header.length; h++) dv.setUint8(h, header.charCodeAt(h));
      dv.setUint32(80, count, true);
      var o = 84;
      for (var t = 0; t < idx.length; t += 3) {
        var a = idx[t] * 3, b = idx[t + 1] * 3, c = idx[t + 2] * 3;
        var ux = p[b] - p[a], uy = p[b + 1] - p[a + 1], uz = p[b + 2] - p[a + 2];
        var vx = p[c] - p[a], vy = p[c + 1] - p[a + 1], vz = p[c + 2] - p[a + 2];
        var nx = uy * vz - uz * vy, ny = uz * vx - ux * vz, nz = ux * vy - uy * vx;
        var l = Math.hypot(nx, ny, nz) || 1;
        dv.setFloat32(o, nx / l, true); dv.setFloat32(o + 4, ny / l, true); dv.setFloat32(o + 8, nz / l, true);
        [a, b, c].forEach(function (k, v) {
          dv.setFloat32(o + 12 + v * 12, p[k], true);
          dv.setFloat32(o + 16 + v * 12, p[k + 1], true);
          dv.setFloat32(o + 20 + v * 12, p[k + 2], true);
        });
        dv.setUint16(o + 48, 0, true);
        o += 50;
      }
      return buffer;
    }

    return {
      exportStl: exportStl,
      dispose: function () {
        disposed = true;
        if (scheme && scheme.removeEventListener) scheme.removeEventListener("change", onScheme);
        if (legendView) legendView.el.remove();
        windowListeners.forEach(function (l) { window.removeEventListener(l[0], l[1]); });
        var lose = gl.getExtension("WEBGL_lose_context");
        if (lose) lose.loseContext();
      }
    };
  }

  window.TeamCraftViewer = { mount: mount };

  // Web pages: <canvas id="viewer-canvas" data-src [data-name]>.
  var canvas = document.getElementById("viewer-canvas");
  if (canvas && canvas.getAttribute("data-src")) {
    var src = canvas.getAttribute("data-src");
    var stlBtn = document.getElementById("viewer-stl");
    var viewer = mount(canvas, {
      statusEl: document.getElementById("viewer-status"),
      resetBtn: document.getElementById("viewer-reset"),
      upBtn: document.getElementById("viewer-up"),
      onReady: function () { if (stlBtn) stlBtn.disabled = false; },
      load: function () {
        return fetch(src, { credentials: "same-origin" }).then(function (r) {
          if (r.ok) return r.arrayBuffer();
          // the server explains why there is no preview (JSON error body)
          return r.json().then(function (body) {
            throw new Error((body && body.error && body.error.message) || "The preview failed (" + r.status + ").");
          }, function () { throw new Error("The preview failed (" + r.status + ")."); });
        });
      }
    });
    // STL export of a STEP/3MF/… model, e.g. for 3D printing.
    if (stlBtn) {
      stlBtn.addEventListener("click", function () {
        var name = (canvas.getAttribute("data-name") || "model").replace(/\.[^.]+$/, "") + ".stl";
        var url = URL.createObjectURL(new Blob([viewer.exportStl()], { type: "model/stl" }));
        var a = document.createElement("a");
        a.href = url;
        a.download = name;
        document.body.appendChild(a);
        a.click();
        a.remove();
        setTimeout(function () { URL.revokeObjectURL(url); }, 10000);
      });
    }
  }
})();
