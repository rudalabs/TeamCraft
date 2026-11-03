// TeamCraft 3D preview: a small self-contained WebGL2 viewer.
//
// * STL (binary and ASCII) is parsed here.
// * STEP / IGES are tessellated by OpenCascade (occt-import-js, WASM) in a
//   dedicated worker (/static/step-worker.js), which has its own CSP so the
//   pages themselves never need 'unsafe-eval'.
//
// Mouse: drag = rotate, wheel = zoom, double-click = reset. Touch: one finger
// rotates, pinch zooms.
(function () {
  "use strict";

  var canvas = document.getElementById("viewer-canvas");
  if (!canvas) return;
  var statusEl = document.getElementById("viewer-status");
  var src = canvas.getAttribute("data-src");
  var format = canvas.getAttribute("data-format");

  function setStatus(text, isError) {
    if (!statusEl) return;
    statusEl.textContent = text;
    statusEl.className = isError ? "flash flash-error" : "small muted";
  }

  // ---------------------------------------------------------------- parsing

  function parseStl(buffer) {
    if (buffer.byteLength >= 84) {
      var dv = new DataView(buffer);
      var n = dv.getUint32(80, true);
      if (84 + n * 50 === buffer.byteLength) return parseBinaryStl(dv, n);
    }
    return parseAsciiStl(new TextDecoder().decode(new Uint8Array(buffer)));
  }

  function parseBinaryStl(dv, n) {
    var pos = new Float32Array(n * 9);
    var nor = new Float32Array(n * 9);
    for (var i = 0; i < n; i++) {
      var o = 84 + i * 50;
      var nx = dv.getFloat32(o, true), ny = dv.getFloat32(o + 4, true), nz = dv.getFloat32(o + 8, true);
      for (var v = 0; v < 3; v++) {
        var p = o + 12 + v * 12, k = i * 9 + v * 3;
        pos[k] = dv.getFloat32(p, true);
        pos[k + 1] = dv.getFloat32(p + 4, true);
        pos[k + 2] = dv.getFloat32(p + 8, true);
        nor[k] = nx; nor[k + 1] = ny; nor[k + 2] = nz;
      }
    }
    return [faceNormals(pos, nor)];
  }

  function parseAsciiStl(text) {
    var re = /vertex\s+([-+0-9.eE]+)\s+([-+0-9.eE]+)\s+([-+0-9.eE]+)/g;
    var values = [], m;
    while ((m = re.exec(text)) !== null) values.push(+m[1], +m[2], +m[3]);
    if (values.length < 9) throw new Error("The STL file contains no triangles.");
    return [faceNormals(new Float32Array(values), null)];
  }

  // Fills in missing or zero normals from the triangle geometry.
  function faceNormals(p, given) {
    var n = given || new Float32Array(p.length);
    for (var i = 0; i + 8 < p.length; i += 9) {
      if (given && Math.hypot(n[i], n[i + 1], n[i + 2]) > 1e-6) continue;
      var ax = p[i + 3] - p[i], ay = p[i + 4] - p[i + 1], az = p[i + 5] - p[i + 2];
      var bx = p[i + 6] - p[i], by = p[i + 7] - p[i + 1], bz = p[i + 8] - p[i + 2];
      var cx = ay * bz - az * by, cy = az * bx - ax * bz, cz = ax * by - ay * bx;
      var l = Math.hypot(cx, cy, cz) || 1;
      for (var v = 0; v < 9; v += 3) {
        n[i + v] = cx / l; n[i + v + 1] = cy / l; n[i + v + 2] = cz / l;
      }
    }
    return { positions: p, normals: n, indices: null, color: null };
  }

  // Smooth vertex normals for indexed meshes without normals.
  function vertexNormals(p, idx) {
    var n = new Float32Array(p.length);
    for (var t = 0; t < idx.length; t += 3) {
      var a = idx[t] * 3, b = idx[t + 1] * 3, c = idx[t + 2] * 3;
      var ux = p[b] - p[a], uy = p[b + 1] - p[a + 1], uz = p[b + 2] - p[a + 2];
      var vx = p[c] - p[a], vy = p[c + 1] - p[a + 1], vz = p[c + 2] - p[a + 2];
      var cx = uy * vz - uz * vy, cy = uz * vx - ux * vz, cz = ux * vy - uy * vx;
      [a, b, c].forEach(function (k) { n[k] += cx; n[k + 1] += cy; n[k + 2] += cz; });
    }
    for (var i = 0; i < n.length; i += 3) {
      var l = Math.hypot(n[i], n[i + 1], n[i + 2]) || 1;
      n[i] /= l; n[i + 1] /= l; n[i + 2] /= l;
    }
    return n;
  }

  function parseCad(buffer, fmt) {
    return new Promise(function (resolve, reject) {
      var worker;
      try {
        worker = new Worker("/static/step-worker.js");
      } catch (e) {
        reject(new Error("This browser cannot start the STEP preview engine."));
        return;
      }
      worker.onmessage = function (e) {
        worker.terminate();
        if (e.data && e.data.error) reject(new Error(e.data.error));
        else resolve(e.data);
      };
      worker.onerror = function (e) {
        worker.terminate();
        reject(new Error(e.message || "The STEP preview engine failed."));
      };
      worker.postMessage({ format: fmt, buffer: buffer }, [buffer]);
    }).then(function (result) {
      if (!result || !result.success || !result.meshes || !result.meshes.length) {
        throw new Error("The file could not be tessellated (no solid geometry found).");
      }
      return result.meshes.map(function (m) {
        var pos = new Float32Array(m.attributes.position.array);
        var idx = new Uint32Array(m.index.array);
        var nor = m.attributes.normal ? new Float32Array(m.attributes.normal.array) : vertexNormals(pos, idx);
        return { positions: pos, normals: nor, indices: idx, color: m.color || null };
      });
    });
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

  function upper3(m) {
    return new Float32Array([m[0], m[1], m[2], m[4], m[5], m[6], m[8], m[9], m[10]]);
  }

  // ---------------------------------------------------------------- colours

  function cssColor(name, fallback) {
    var raw = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
    var m = /^#([0-9a-f]{6})$/i.exec(raw);
    if (!m) return fallback;
    var v = parseInt(m[1], 16);
    return [((v >> 16) & 255) / 255, ((v >> 8) & 255) / 255, (v & 255) / 255];
  }

  // ---------------------------------------------------------------- renderer

  var gl = canvas.getContext("webgl2", { antialias: true });
  if (!gl) {
    setStatus("Your browser does not support WebGL 2 – download the file to view it.", true);
    return;
  }

  var VERT = "#version 300 es\nin vec3 aPos; in vec3 aNor; uniform mat4 uMvp; uniform mat3 uRot; out vec3 vNor;\n" +
    "void main() { vNor = uRot * aNor; gl_Position = uMvp * vec4(aPos, 1.0); }";
  var FRAG = "#version 300 es\nprecision highp float; in vec3 vNor; uniform vec3 uColor; out vec4 frag;\n" +
    "void main() { vec3 n = normalize(vNor); if (!gl_FrontFacing) n = -n;\n" +
    "  float d = max(dot(n, normalize(vec3(0.35, 0.55, 1.0))), 0.0);\n" +
    "  float rim = pow(1.0 - max(n.z, 0.0), 3.0) * 0.18;\n" +
    "  frag = vec4(uColor * (0.25 + 0.75 * d) + rim, 1.0); }";

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
    nor: gl.getAttribLocation(program, "aNor"),
    mvp: gl.getUniformLocation(program, "uMvp"),
    rot: gl.getUniformLocation(program, "uRot"),
    color: gl.getUniformLocation(program, "uColor")
  };

  var drawables = [];
  var view = { yaw: -0.6, pitch: 0.45, dist: 1, center: [0, 0, 0], radius: 1 };
  var home = null;
  var baseColor = cssColor("--c3", [0.42, 0.7, 0.88]);
  var clearColor = cssColor("--bg-raised", [0.03, 0.09, 0.14]);

  function upload(mesh) {
    var vao = gl.createVertexArray();
    gl.bindVertexArray(vao);
    var pb = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, pb);
    gl.bufferData(gl.ARRAY_BUFFER, mesh.positions, gl.STATIC_DRAW);
    gl.enableVertexAttribArray(loc.pos);
    gl.vertexAttribPointer(loc.pos, 3, gl.FLOAT, false, 0, 0);
    var nb = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, nb);
    gl.bufferData(gl.ARRAY_BUFFER, mesh.normals, gl.STATIC_DRAW);
    gl.enableVertexAttribArray(loc.nor);
    gl.vertexAttribPointer(loc.nor, 3, gl.FLOAT, false, 0, 0);
    var count = mesh.positions.length / 3;
    if (mesh.indices) {
      var ib = gl.createBuffer();
      gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, ib);
      gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, mesh.indices, gl.STATIC_DRAW);
      count = mesh.indices.length;
    }
    gl.bindVertexArray(null);
    return { vao: vao, count: count, indexed: !!mesh.indices, color: mesh.color };
  }

  function frame(meshes) {
    var min = [Infinity, Infinity, Infinity], max = [-Infinity, -Infinity, -Infinity];
    var triangles = 0;
    meshes.forEach(function (m) {
      var p = m.positions;
      for (var i = 0; i < p.length; i += 3) {
        for (var a = 0; a < 3; a++) {
          if (p[i + a] < min[a]) min[a] = p[i + a];
          if (p[i + a] > max[a]) max[a] = p[i + a];
        }
      }
      triangles += (m.indices ? m.indices.length : p.length / 3) / 3;
    });
    var size = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    view.center = [(min[0] + max[0]) / 2, (min[1] + max[1]) / 2, (min[2] + max[2]) / 2];
    view.radius = Math.max(Math.hypot(size[0], size[1], size[2]) / 2, 1e-6);
    view.dist = view.radius * 2.8;
    home = { yaw: view.yaw, pitch: view.pitch, dist: view.dist };
    function fmt(v) { return (Math.round(v * 100) / 100).toString(); }
    setStatus(Math.round(triangles).toLocaleString() + " triangles · " + fmt(size[0]) + " × " + fmt(size[1]) +
      " × " + fmt(size[2]) + " (model units) · drag to rotate, wheel to zoom, double-click to reset");
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
    if (!drawables.length) return;
    var rot = multiply(rotationX(view.pitch), rotationY(view.yaw));
    var c = view.center;
    var model = multiply(rot, translation(-c[0], -c[1], -c[2]));
    var viewM = multiply(translation(0, 0, -view.dist), model);
    var proj = perspective(Math.PI / 4, w / h, Math.max(view.dist - view.radius * 4, view.radius / 100), view.dist + view.radius * 4);
    gl.useProgram(program);
    gl.uniformMatrix4fv(loc.mvp, false, multiply(proj, viewM));
    gl.uniformMatrix3fv(loc.rot, false, upper3(rot));
    drawables.forEach(function (d) {
      gl.uniform3fv(loc.color, d.color || baseColor);
      gl.bindVertexArray(d.vao);
      if (d.indexed) gl.drawElements(gl.TRIANGLES, d.count, gl.UNSIGNED_INT, 0);
      else gl.drawArrays(gl.TRIANGLES, 0, d.count);
    });
    gl.bindVertexArray(null);
  }

  var pending = false;
  function redraw() {
    if (pending) return;
    pending = true;
    requestAnimationFrame(function () { pending = false; render(); });
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
      if (pinch) view.dist *= pinch / Math.max(d, 1);
      pinch = d;
    } else {
      view.yaw += (e.clientX - prev.x) * 0.01;
      view.pitch = Math.max(-1.55, Math.min(1.55, view.pitch + (e.clientY - prev.y) * 0.01));
    }
    pointers[e.pointerId] = { x: e.clientX, y: e.clientY };
    redraw();
  });
  function release(e) { delete pointers[e.pointerId]; pinch = null; }
  canvas.addEventListener("pointerup", release);
  canvas.addEventListener("pointercancel", release);
  canvas.addEventListener("wheel", function (e) {
    e.preventDefault();
    view.dist *= Math.exp(e.deltaY * 0.0012);
    redraw();
  }, { passive: false });
  canvas.addEventListener("dblclick", function () {
    if (home) { view.yaw = home.yaw; view.pitch = home.pitch; view.dist = home.dist; redraw(); }
  });
  window.addEventListener("resize", redraw);

  // ---------------------------------------------------------------- load

  setStatus("Loading " + (format || "file").toUpperCase() + " …");
  render();
  fetch(src, { credentials: "same-origin" })
    .then(function (r) {
      if (!r.ok) throw new Error("Download failed (" + r.status + ").");
      return r.arrayBuffer();
    })
    .then(function (buffer) {
      if (format === "stl") return parseStl(buffer);
      setStatus("Tessellating " + format.toUpperCase() + " with OpenCascade …");
      return parseCad(buffer, format);
    })
    .then(function (meshes) {
      frame(meshes);
      drawables = meshes.map(upload);
      redraw();
    })
    .catch(function (err) {
      setStatus(err.message || String(err), true);
    });
})();
