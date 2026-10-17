// Quintess site: platform downloads, release details from GitHub, and a
// cookieless page-view count.
(() => {
  "use strict";
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

  // ---- Header: scroll progress ----
  const progress = $(".progress span");
  const onScroll = () => {
    const max = document.documentElement.scrollHeight - innerHeight;
    progress?.style.setProperty("--progress", max > 0 ? (scrollY / max).toFixed(4) : 0);
  };
  addEventListener("scroll", onScroll, { passive: true });
  onScroll();

  // ---- Release details from GitHub (through this site, cached) ----
  // Each download link and details block is marked with its package kind
  // (windows, deb or tarball); the page carries working links for 0.4.4 if
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
      if (other) {
        primary.querySelector("a").href = other.href;
        const name = decodeURIComponent(new URL(other.href).pathname.split("/").pop());
        const size = $("[data-asset-size]", other)?.textContent;
        $("[data-asset-line]", primary).textContent = name + (size ? ` · ${size}` : "");
      }
      if (key.startsWith("windows")) {
        $$("[data-recommended]").forEach((label) => {
          label.hidden = label.dataset.recommended !== key;
          if (!label.hidden) label.textContent = "Recommended for your PC";
        });
        $$(".windows .dl-file").forEach((link) => link.classList.toggle("first", link.dataset.asset === key));
      }
    };
    const agent = navigator.userAgent;
    if (/Android|iPhone|iPad|iPod|CrOS/.test(agent) || (/Macintosh/.test(agent) && navigator.maxTouchPoints > 1)) {
      primary.removeAttribute("data-asset");
      const link = primary.querySelector("a");
      link.href = "#download";
      link.removeAttribute("data-download");
      link.removeAttribute("data-track");
      $("[data-primary-label]", primary).textContent = "Choose a desktop download";
      $("[data-primary-note]", primary).textContent = "For Windows, Mac and Linux computers";
      $("[data-asset-line]", primary).hidden = true;
    }
    else if (/Macintosh|Mac OS X/.test(agent)) choose("macos");
    else if (/Linux/.test(agent) && !/Android/.test(agent)) choose("appimage");
    // Chromium browsers can say whether Windows runs on ARM.
    else if (/Windows/.test(agent) && navigator.userAgentData?.getHighEntropyValues) {
      navigator.userAgentData.getHighEntropyValues(["architecture"]).then((values) => { if (values.architecture === "arm") choose("windows_arm64"); }).catch(() => {});
    }
  }

  // Platform panels: one open at a time; all are visible without JavaScript.
  const downloads = $("#dl");
  if (downloads) {
    downloads.classList.add("live");
    const cards = $$(".dl-card", downloads);
    const ownOrder = cards.slice();
    const headOf = (card) => card.querySelector(".dl-head");
    const choose = (chosen) => {
      const opens = chosen && !chosen.classList.contains("open");
      cards.forEach((card) => {
        const on = opens && card === chosen;
        card.classList.toggle("open", on);
        headOf(card).setAttribute("aria-expanded", on ? "true" : "false");
      });
      // The open panel goes first, above the other platforms.
      if (opens) downloads.insertBefore(chosen, downloads.firstChild);
      else ownOrder.forEach((card) => downloads.appendChild(card));
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
    // The card for this visitor's system opens automatically. Phones and
    // tablets get no mark: there is no version for them.
    const platform = navigator.userAgentData?.platform || "";
    const agent = navigator.userAgent || "";
    const both = `${platform} ${agent}`;
    let system = null;
    if (/Android|iPhone|iPad|iPod|CrOS/i.test(both) || (/Macintosh/.test(agent) && navigator.maxTouchPoints > 1)) system = null;
    else if (/^Windows$/i.test(platform) || (!platform && /Windows NT/.test(agent))) system = "windows";
    else if (/^macOS$/i.test(platform) || (!platform && /Macintosh|Mac OS X/.test(agent))) system = "macos";
    else if (/^Linux$/i.test(platform) || (!platform && /Linux|X11/.test(agent))) system = "linux";
    const yours = system && downloads.querySelector(`.dl-card[data-system="${system}"]`);
    if (yours) {
      yours.classList.add("yours");
      yours.querySelector(".dl-tag").hidden = false;
      yours.classList.add("open");
      headOf(yours).setAttribute("aria-expanded", "true");
      downloads.insertBefore(yours, downloads.firstChild);
    }
  }

  fetch("/api/release").then((response) => (response.ok ? response.json() : null)).then((release) => {
    if (!release || !release.assets) return;
    $$("[data-version]").forEach((element) => (element.textContent = release.version));
    const date = new Date(release.published).toLocaleDateString("en-US", { year: "numeric", month: "short", day: "numeric" });
    if (!Number.isNaN(Date.parse(release.published))) $$("[data-release-date]").forEach((element) => (element.textContent = ` · released ${date}`));
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
