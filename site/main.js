/* VoiceWriter landing page — progressive-enhancement helpers.
   Nothing here is required for the page to render or the downloads to work. */

(function () {
  "use strict";

  var reduceMotion =
    window.matchMedia &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  document.documentElement.classList.add("js");

  /* ---- Scroll-driven chrome: header shadow, progress bar, demo drift --- */
  var header = document.getElementById("siteHeader");
  var progress = document.getElementById("scrollProgress");
  var demoFrame = document.querySelector(".demo-frame");
  var hero = document.querySelector(".hero");
  var ticking = false;

  var onScroll = function () {
    var y = window.scrollY || window.pageYOffset;
    if (header) header.classList.toggle("scrolled", y > 8);
    if (progress) {
      var max = document.documentElement.scrollHeight - window.innerHeight;
      var p = max > 0 ? Math.min(y / max, 1) : 0;
      progress.style.transform = "scaleX(" + p.toFixed(4) + ")";
    }
    if (demoFrame && hero && !reduceMotion) {
      var heroBottom = hero.offsetTop + hero.offsetHeight;
      if (y < heroBottom) {
        demoFrame.style.setProperty("--demo-shift", (y * -0.04).toFixed(1) + "px");
      }
    }
    ticking = false;
  };
  var requestScroll = function () {
    if (!ticking) {
      window.requestAnimationFrame(onScroll);
      ticking = true;
    }
  };
  onScroll();
  window.addEventListener("scroll", requestScroll, { passive: true });
  window.addEventListener("resize", requestScroll, { passive: true });

  /* ---- Fill download buttons from the latest GitHub release ----------- */
  var REPO = "Xrenes/voicewriter";
  function mb(bytes) {
    return bytes ? "~" + (bytes / 1048576).toFixed(1) + " MB" : "";
  }
  function fillDownloads(data) {
    var assets = (data && data.assets) || [];
    var exe = assets.find(function (a) {
      return /\.exe$/i.test(a.name);
    });
    var msi = assets.find(function (a) {
      return /\.msi$/i.test(a.name);
    });
    var set = function (id, txt) {
      var el = document.getElementById(id);
      if (el && txt) el.textContent = txt;
    };
    var link = function (id, href) {
      var el = document.getElementById(id);
      if (el && href) el.setAttribute("href", href);
    };
    if (exe) {
      link("dlExe", exe.browser_download_url);
      set("exeSize", mb(exe.size));
      set("exeSizeSub", mb(exe.size));
    }
    if (msi) {
      link("dlMsi", msi.browser_download_url);
      set("msiSize", mb(msi.size));
      set("msiSizeSub", mb(msi.size));
    }
  }
  if (window.fetch) {
    fetch("https://api.github.com/repos/" + REPO + "/releases/latest", {
      headers: { Accept: "application/vnd.github+json" },
    })
      .then(function (r) {
        return r.ok ? r.json() : null;
      })
      .then(function (data) {
        if (data) fillDownloads(data);
      })
      .catch(function () {
        /* keep the hard-coded /releases/latest/download/... links */
      });
  }

  /* ---- Reveal-on-scroll (with a stagger inside each group) ------------- */
  var revealTargets = document.querySelectorAll(
    ".section-head, .step, .feature, .trust-item, .download-panel, .faq"
  );
  revealTargets.forEach(function (el) {
    el.classList.add("reveal");
  });
  var stagger = function (selector, groupSelector) {
    document.querySelectorAll(groupSelector).forEach(function (group) {
      group.querySelectorAll(selector).forEach(function (el, i) {
        el.style.setProperty("--reveal-delay", i * 80 + "ms");
      });
    });
  };
  stagger(".step", ".steps");
  stagger(".feature", ".feature-grid");
  stagger(".trust-item", ".trust");

  var revealAll = function () {
    revealTargets.forEach(function (el) {
      el.classList.add("in");
    });
  };
  if (!reduceMotion && "IntersectionObserver" in window) {
    var io = new IntersectionObserver(
      function (entries) {
        entries.forEach(function (entry) {
          if (entry.isIntersecting) {
            entry.target.classList.add("in");
            io.unobserve(entry.target);
          }
        });
      },
      { threshold: 0.12, rootMargin: "0px 0px -8% 0px" }
    );
    revealTargets.forEach(function (el) {
      io.observe(el);
    });
    setTimeout(revealAll, 2500);
  } else {
    revealAll();
  }

  /* ---- Hero demo: the type-as-you-speak loop ------------------------- */
  var script = document.getElementById("demoScript");
  var cursor = document.getElementById("demoCursor");
  if (script) {
    var words = Array.prototype.slice.call(script.querySelectorAll(".w"));
    var placeCursor = function (i) {
      if (!cursor) return;
      var ref = words[i];
      if (ref && ref.parentNode) {
        ref.parentNode.insertBefore(cursor, ref.nextSibling);
      }
    };
    if (reduceMotion) {
      var stop = Math.ceil(words.length * 0.55);
      words.forEach(function (w, i) {
        if (i < stop) w.classList.add("read");
      });
      if (words[stop]) words[stop].classList.add("current");
      placeCursor(stop);
      return;
    }
    var idx = 0;
    var step = function () {
      words.forEach(function (w) {
        w.classList.remove("current");
      });
      if (idx < words.length) {
        var w = words[idx];
        w.classList.add("read", "current");
        placeCursor(idx);
        idx++;
        setTimeout(step, 90 + Math.random() * 170);
      } else {
        setTimeout(function () {
          words.forEach(function (x) {
            x.classList.remove("read", "current");
          });
          idx = 0;
          placeCursor(0);
          setTimeout(step, 900);
        }, 2200);
      }
    };
    setTimeout(step, 1200);
  }
})();
