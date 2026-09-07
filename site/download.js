// Fill the download buttons from the latest GitHub release so the site always
// points at the newest installers without a rebuild.
(function () {
  var REPO = "Xrenes/voicewriter";
  var RELEASES = "https://github.com/" + REPO + "/releases";

  // Sensible fallbacks (latest known assets) if the API call fails or is rate-limited.
  var fallback = {
    tag: "v0.1.0",
    exe: RELEASES + "/latest/download/VoiceWriter_0.1.0_x64-setup.exe",
    msi: RELEASES + "/latest/download/VoiceWriter_0.1.0_x64_en-US.msi",
  };

  function setLink(id, href) {
    var el = document.getElementById(id);
    if (el && href) el.setAttribute("href", href);
  }
  function setText(id, txt) {
    var el = document.getElementById(id);
    if (el && txt != null) el.textContent = txt;
  }
  function mb(bytes) {
    return bytes ? (bytes / 1048576).toFixed(1) + " MB" : "";
  }

  function apply(data) {
    var tag = (data && data.tag_name) || fallback.tag;
    var assets = (data && data.assets) || [];
    var exe = assets.find(function (a) {
      return /\.exe$/i.test(a.name);
    });
    var msi = assets.find(function (a) {
      return /\.msi$/i.test(a.name);
    });

    setLink("dlExe", exe ? exe.browser_download_url : fallback.exe);
    setLink("dlMsi", msi ? msi.browser_download_url : fallback.msi);
    setText("exeSize", exe ? mb(exe.size) : "");
    setText("msiSize", msi ? mb(msi.size) : "");
    setText("relVer", tag);
    setText("relVerFoot", tag);
    setLink("relLink", RELEASES);
  }

  // Start with fallbacks so buttons work immediately.
  apply(null);

  fetch("https://api.github.com/repos/" + REPO + "/releases/latest", {
    headers: { Accept: "application/vnd.github+json" },
  })
    .then(function (r) {
      return r.ok ? r.json() : null;
    })
    .then(function (data) {
      if (data) apply(data);
    })
    .catch(function () {
      /* keep fallbacks */
    });
})();
