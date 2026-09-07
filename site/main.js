/* VoiceWriter landing page — small progressive-enhancement helpers.
   The page renders and the downloads work without any of this. */

(function () {
  "use strict";

  var reduceMotion =
    window.matchMedia &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  document.documentElement.classList.add("js");

  /* ---- Header shadow on scroll ---- */
  var header = document.getElementById("siteHeader");
  var onScroll = function () {
    if (header) header.classList.toggle("scrolled", (window.scrollY || 0) > 8);
  };
  onScroll();
  window.addEventListener("scroll", onScroll, { passive: true });

  /* ---- Fill download buttons with live file sizes from GitHub ---- */
  var REPO = "Xrenes/voicewriter";
  function mb(bytes) {
    return bytes ? "~" + (bytes / 1048576).toFixed(1) + " MB" : "";
  }
  if (window.fetch) {
    fetch("https://api.github.com/repos/" + REPO + "/releases/latest", {
      headers: { Accept: "application/vnd.github+json" },
    })
      .then(function (r) {
        return r.ok ? r.json() : null;
      })
      .then(function (data) {
        if (!data) return;
        var assets = data.assets || [];
        var exe = assets.find(function (a) {
          return /\.exe$/i.test(a.name);
        });
        var msi = assets.find(function (a) {
          return /\.msi$/i.test(a.name);
        });
        var apply = function (id, sizeId, asset) {
          if (!asset) return;
          var a = document.getElementById(id);
          var s = document.getElementById(sizeId);
          if (a) a.setAttribute("href", asset.browser_download_url);
          if (s) s.textContent = mb(asset.size);
        };
        apply("dlExe", "exeSize", exe);
        apply("dlMsi", "msiSize", msi);
      })
      .catch(function () {
        /* fall back to the hard-coded /releases/latest/download/... links */
      });
  }

  /* ---- Reveal-on-scroll ---- */
  var targets = document.querySelectorAll(
    ".slab-head, .flow li, .card, .get-card"
  );
  targets.forEach(function (el) {
    el.classList.add("reveal");
  });
  var group = function (sel, parent) {
    document.querySelectorAll(parent).forEach(function (g) {
      g.querySelectorAll(sel).forEach(function (el, i) {
        el.style.setProperty("--reveal-delay", i * 70 + "ms");
      });
    });
  };
  group(".flow li", ".flow");
  group(".card", ".cards");

  var showAll = function () {
    targets.forEach(function (el) {
      el.classList.add("in");
    });
  };
  if (!reduceMotion && "IntersectionObserver" in window) {
    var io = new IntersectionObserver(
      function (entries) {
        entries.forEach(function (e) {
          if (e.isIntersecting) {
            e.target.classList.add("in");
            io.unobserve(e.target);
          }
        });
      },
      { threshold: 0.14, rootMargin: "0px 0px -6% 0px" }
    );
    targets.forEach(function (el) {
      io.observe(el);
    });
    setTimeout(showAll, 2500);
  } else {
    showAll();
  }

  /* ---- Signature stage: type a sentence, then dim the waveform ---- */
  var stage = document.querySelector(".stage");
  var typed = document.getElementById("typed");
  var caret = typed ? typed.querySelector(".caret") : null;
  if (typed && caret) {
    var line = "Hold the key and speak — your words land right here.";

    var runOnce = function () {
      typed.textContent = "";
      typed.appendChild(caret);
      if (stage) stage.classList.remove("done");

      if (reduceMotion) {
        typed.insertBefore(document.createTextNode(line), caret);
        if (stage) stage.classList.add("done");
        return;
      }

      var i = 0;
      var tick = function () {
        if (i < line.length) {
          typed.insertBefore(document.createTextNode(line.charAt(i)), caret);
          i++;
          var ch = line.charAt(i - 1);
          var delay = ch === " " ? 70 : 30 + Math.random() * 55;
          setTimeout(tick, delay);
        } else {
          if (stage) stage.classList.add("done");
          setTimeout(runOnce, 3200);
        }
      };
      setTimeout(tick, 900);
    };

    runOnce();
  }
})();
