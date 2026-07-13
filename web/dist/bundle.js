// Placeholder bundle (Milestone 1). Replaced by the esbuild React build.
// Reads the compressed report-data blob, inflates + JSON.parses it, and dumps
// the JSON into a <pre> so the self-contained embedding + bootstrap can be
// verified before the real UI lands.
(function () {
  var b64 = window.__HPROF_DATA_B64__ || "";
  window.hprofDecodeText(b64).then(function (json) {
    var root = document.getElementById("root");
    if (root) {
      root.textContent = "";
      var pre = document.createElement("pre");
      pre.textContent = JSON.stringify(JSON.parse(json), null, 2);
      root.appendChild(pre);
    }
  }).catch(function (e) {
    var root = document.getElementById("root");
    if (root) root.textContent = "Failed to parse report data: " + e;
  });
})();
