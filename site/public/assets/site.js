// Quintess site: scroll reveals, release details from GitHub, and a
// cookieless page-view count.
(() => {
  "use strict";
  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const $ = (selector, root = document) => root.querySelector(selector);
  const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];
  // ---- Page-view count (first party, no cookies, see /privacy/) ----
  function track(kind, extra = {}) {
    // Browsers that ask not to be tracked are not counted.
    if (navigator.globalPrivacyControl || navigator.doNotTrack === "1") return;
    // Only the referring site's address is sent, never the full page it came from.
    let referrer = "";
    try { referrer = document.referrer ? new URL(document.referrer).origin : ""; } catch {}
    const body = JSON.stringify({ kind, path: location.pathname, referrer, ...extra });
    try {
      if (!navigator.sendBeacon("/api/hit", new Blob([body], { type: "application/json" }))) throw new Error();
    } catch {
      fetch("/api/hit", { method: "POST", body, keepalive: true, headers: { "content-type": "application/json" } }).catch(() => {});
    }
  }
  track("view");
  document.addEventListener("click", (event) => {
    const link = event.target.closest("[data-track]");
    if (link) track(link.dataset.track);
  });

  // ---- Header: scroll progress and the pointer readout in PDF points ----
  const progress = $(".progress span");
  const onScroll = () => {
    const max = document.documentElement.scrollHeight - innerHeight;
    progress?.style.setProperty("--progress", max > 0 ? (scrollY / max).toFixed(4) : 0);
  };
  addEventListener("scroll", onScroll, { passive: true });
  onScroll();

  const readout = $("[data-readout]");
  const crosshair = $(".crosshair");
  if (window.matchMedia("(pointer: fine) and (min-width: 1061px)").matches && !reducedMotion) {
    addEventListener("pointermove", (event) => {
      crosshair.classList.add("on");
      crosshair.style.setProperty("--x", event.clientX + "px");
      crosshair.style.setProperty("--y", event.clientY + "px");
      // 96 CSS pixels to the inch, 72 points to the inch.
      const toPoints = (pixels) => String(Math.round(pixels * 0.75)).padStart(4, "0");
      if (readout) readout.textContent = `x ${toPoints(event.pageX)} · y ${toPoints(event.pageY)} pt`;
    }, { passive: true });
    document.addEventListener("pointerleave", () => crosshair.classList.remove("on"));
  }

  // ---- Reveal sections when they come into view ----
  const whenVisible = (element, callback, threshold = 0.25) => {
    if (!("IntersectionObserver" in window)) return callback(true);
    new IntersectionObserver((entries) => entries.forEach((entry) => callback(entry.isIntersecting)), { threshold }).observe(element);
  };
  $$(".sec-head, .diff-grid .cell, .bench, .faq, .download-card").forEach((element) => {
    element.classList.add("reveal");
    whenVisible(element, (visible) => visible && element.classList.add("in"), 0.12);
  });

  // The big wordmark fills in as it scrolls into view.
  const giant = $(".giant span");
  if (giant) {
    const fill = () => {
      const rect = giant.getBoundingClientRect();
      const share = Math.min(1, Math.max(0, (innerHeight - rect.top) / (innerHeight * 0.7)));
      giant.style.setProperty("--fill", (reducedMotion ? 100 : share * 100).toFixed(1) + "%");
    };
    addEventListener("scroll", fill, { passive: true });
    fill();
  }

  // ---- Release details from GitHub (through this site, cached) ----
  // Each download link and details block is marked with its package kind
  // (windows, deb or tarball); the page carries working links for 0.2.0 if
  // this request fails.
  // The hero's button offers the package for the visitor's system.
  const primary = $("[data-primary-download]");
  if (primary) {
    const choices = {
      windows: ["Download for Windows", "Windows 10/11 · x86_64 · installs for your user account"],
      windows_arm64: ["Download for Windows on ARM", "Windows 11 on ARM · native ARM64"],
      macos: ["Download for Mac", "macOS 11 or later · Apple silicon and Intel"],
      appimage: ["Download for Linux", "AppImage · most distributions · runs without installing"],
    };
    const choose = (key) => {
      if (!choices[key]) return;
      primary.dataset.asset = key;
      primary.querySelector("[data-download]").dataset.download = key;
      $("[data-primary-label]", primary).textContent = choices[key][0];
      $("[data-primary-note]", primary).textContent = choices[key][1];
      const other = document.querySelector(`.dl-others [data-asset="${key}"]`);
      if (other) primary.querySelector("a").href = other.href;
    };
    const agent = navigator.userAgent;
    if (/Macintosh|Mac OS X/.test(agent) && !/iPhone|iPad/.test(agent)) choose("macos");
    else if (/Linux/.test(agent) && !/Android/.test(agent)) choose("appimage");
    // Chromium browsers can say whether Windows runs on ARM.
    else if (/Windows/.test(agent) && navigator.userAgentData?.getHighEntropyValues) {
      navigator.userAgentData.getHighEntropyValues(["architecture"]).then((values) => { if (values.architecture === "arm") choose("windows_arm64"); }).catch(() => {});
    }
  }

  fetch("/api/release").then((response) => (response.ok ? response.json() : null)).then((release) => {
    if (!release || !release.assets) return;
    $$("[data-version]").forEach((element) => (element.textContent = release.version));
    const date = new Date(release.published).toLocaleDateString("en-US", { year: "numeric", month: "short", day: "numeric" });
    $$("[data-asset]").forEach((block) => {
      const asset = release.assets[block.dataset.asset];
      if (!asset) return;
      const megabytes = (asset.size / 1048576).toFixed(1) + " MB";
      const set = (selector, text) => $$(selector, block).forEach((element) => (element.textContent = text));
      $$("[data-download]", block).forEach((link) => (link.href = asset.url));
      set("[data-asset-name]", asset.name);
      set("[data-asset-size]", megabytes);
      set("[data-asset-date]", date);
      if (asset.sha256) set("[data-asset-sha]", asset.sha256);
      set("[data-asset-line]", `${asset.name} · ${megabytes}`);
    });
  }).catch(() => {});

  // Links to a question in the FAQ open it.
  const openFaq = () => {
    const target = location.hash && document.getElementById(location.hash.slice(1));
    if (target && target.tagName === "DETAILS") target.open = true;
  };
  addEventListener("hashchange", openFaq);
  openFaq();
})();

// ---- The guide: filter topics, and mark the topic being read ----
(() => {
  const search = document.querySelector("[data-doc-search]");
  if (!search) return;
  const topics = [...document.querySelectorAll(".doc-topic")];
  const links = new Map([...document.querySelectorAll(".doc-toc a")].map((link) => [link.hash.slice(1), link]));
  search.addEventListener("input", () => {
    const query = search.value.trim().toLowerCase();
    topics.forEach((topic) => {
      const matches = !query || topic.textContent.toLowerCase().includes(query);
      topic.hidden = !matches;
      links.get(topic.id)?.parentElement.toggleAttribute("hidden", !matches);
    });
    const empty = document.querySelector("[data-doc-empty]");
    if (empty) empty.hidden = topics.some((topic) => !topic.hidden);
  });
  if (!("IntersectionObserver" in window)) return;
  const observer = new IntersectionObserver((entries) => {
    entries.forEach((entry) => {
      if (!entry.isIntersecting) return;
      links.forEach((link) => link.classList.remove("current"));
      links.get(entry.target.id)?.classList.add("current");
    });
  }, { rootMargin: "-20% 0px -70% 0px" });
  topics.forEach((topic) => observer.observe(topic));
})();
