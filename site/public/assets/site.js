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
      const other = document.querySelector(`#dl a[data-asset="${key}"]`);
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

  // ---- Download cards (adapted from the Cartogenesis site) ----
  // The one chosen opens across the row and the others move beneath it. The
  // move measures where every card was, lets the grid lay them out anew, and
  // slides each from its old place and size to its new one; each card's
  // contents are scaled the other way at every step, so the words keep their
  // shape while the card around them changes its.
  const downloads = $("#dl");
  if (downloads) {
    downloads.classList.add("live");
    const cards = $$(".dl-card", downloads);
    const ownOrder = cards.slice();
    const lessMotion = matchMedia("(prefers-reduced-motion: reduce)");
    const CARD_MOVE_MS = 420;
    const CARD_MOVE_STEPS = 12;
    const lerp = (from, to, share) => from + (to - from) * share;
    const easeOut = (share) => 1 - Math.pow(1 - share, 3);
    const headOf = (card) => card.querySelector(".dl-head");
    const rearrange = (change) => {
      const before = new Map(cards.map((card) => [card, card.getBoundingClientRect()]));
      change();
      if (lessMotion.matches || !Element.prototype.animate) return;
      cards.forEach((card) => {
        const was = before.get(card), now = card.getBoundingClientRect();
        if (!now.width || !now.height) return;
        const dx = was.left - now.left, dy = was.top - now.top;
        const across = was.width / now.width, down = was.height / now.height;
        if (Math.abs(dx) < 1 && Math.abs(dy) < 1 && Math.abs(across - 1) < 0.01 && Math.abs(down - 1) < 0.01) return;
        const outer = [], inner = [];
        for (let step = 0; step <= CARD_MOVE_STEPS; step++) {
          const share = easeOut(step / CARD_MOVE_STEPS);
          const scaleAcross = lerp(across, 1, share), scaleDown = lerp(down, 1, share);
          outer.push({ offset: step / CARD_MOVE_STEPS, transformOrigin: "0 0", transform: `translate(${lerp(dx, 0, share)}px,${lerp(dy, 0, share)}px) scale(${scaleAcross},${scaleDown})` });
          inner.push({ offset: step / CARD_MOVE_STEPS, transform: `scale(${1 / scaleAcross},${1 / scaleDown})` });
        }
        card.animate(outer, { duration: CARD_MOVE_MS, easing: "linear" });
        card.querySelector(".dl-inner").animate(inner, { duration: CARD_MOVE_MS, easing: "linear" });
        if (card.classList.contains("open")) {
          card.querySelector(".dl-body").animate([{ opacity: 0 }, { opacity: 0, offset: 0.45 }, { opacity: 1 }], { duration: CARD_MOVE_MS + 60 });
        }
      });
    };
    const choose = (chosen) => {
      const opens = chosen && !chosen.classList.contains("open");
      rearrange(() => {
        cards.forEach((card) => {
          const on = opens && card === chosen;
          card.classList.toggle("open", on);
          headOf(card).setAttribute("aria-expanded", on ? "true" : "false");
        });
        // The open card goes first, so it spans the row above the others.
        if (opens) downloads.insertBefore(chosen, downloads.firstChild);
        else ownOrder.forEach((card) => downloads.appendChild(card));
      });
    };
    cards.forEach((card) => {
      headOf(card).setAttribute("aria-expanded", "false");
      headOf(card).addEventListener("click", () => choose(card));
    });
    document.addEventListener("keydown", (event) => {
      if (event.key !== "Escape") return;
      const open = downloads.querySelector(".dl-card.open");
      if (!open) return;
      const focusWasInside = open.contains(document.activeElement);
      choose(null);
      if (focusWasInside) headOf(open).focus();
    });
    $$(".dl-copy", downloads).forEach((button) => {
      button.hidden = false;
      button.addEventListener("click", () => {
        const source = document.getElementById(button.dataset.copy);
        const said = (words) => { button.textContent = words; setTimeout(() => (button.textContent = "Copy"), 1600); };
        const selectInstead = () => {
          const range = document.createRange();
          range.selectNodeContents(source);
          const selection = getSelection();
          selection.removeAllRanges();
          selection.addRange(range);
          said("Selected");
        };
        if (navigator.clipboard?.writeText) navigator.clipboard.writeText(source.textContent).then(() => said("Copied"), selectInstead);
        else selectInstead();
      });
    });
    // The card for this visitor's system is marked, not opened. Phones and
    // tablets get no mark: there is no version for them.
    const platform = navigator.userAgentData?.platform || "";
    const agent = navigator.userAgent || "";
    const both = `${platform} ${agent}`;
    let system = null;
    if (/Android|iPhone|iPad|iPod|CrOS/i.test(both)) system = null;
    else if (/^Windows$/i.test(platform) || (!platform && /Windows NT/.test(agent))) system = "windows";
    else if (/^macOS$/i.test(platform) || (!platform && /Macintosh|Mac OS X/.test(agent))) system = "macos";
    else if (/^Linux$/i.test(platform) || (!platform && /Linux|X11/.test(agent))) system = "linux";
    const yours = system && downloads.querySelector(`.dl-card[data-system="${system}"]`);
    if (yours) {
      yours.classList.add("yours");
      yours.querySelector(".dl-tag").hidden = false;
    }
    // "Mac, Linux, Windows on ARM" from the hero opens nothing by itself;
    // following a link to #download keeps the cards as they are.
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
      if (block.matches("[data-download]")) block.href = asset.url;
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
