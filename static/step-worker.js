// STEP / IGES tessellation for the TeamCraft 3D preview.
//
// Runs OpenCascade (occt-import-js, LGPL-2.1, compiled to WebAssembly) in a
// dedicated worker. The worker script is served with its own Content-Security-
// Policy that allows WebAssembly and the dynamic code occt-import-js needs;
// the pages keep their strict policy. The engine files are not part of the
// repository: they are installed into TEAMCRAFT_VENDOR_DIR (see DEPLOYMENT.md).
"use strict";

var BASE = "/static/vendor/occt-import-js/";
var engine = null;

function loadEngine() {
  if (!engine) {
    importScripts(BASE + "occt-import-js.js");
    // eslint-disable-next-line no-undef
    engine = occtimportjs({ locateFile: function (path) { return BASE + path; } });
  }
  return engine;
}

self.onmessage = function (event) {
  var message = event.data || {};
  var ready;
  try {
    ready = loadEngine();
  } catch (err) {
    self.postMessage({ error: "The STEP/IGES preview engine is not installed on this server – download the file to view it." });
    return;
  }
  ready
    .then(function (occt) {
      var content = new Uint8Array(message.buffer);
      var result = message.format === "iges" ? occt.ReadIgesFile(content, null) : occt.ReadStepFile(content, null);
      self.postMessage(result);
    })
    .catch(function (err) {
      self.postMessage({ error: "STEP/IGES conversion failed: " + ((err && err.message) || err) });
    });
};
