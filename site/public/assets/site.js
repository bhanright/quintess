// Quintess site: the scripted hero demo, the small live demos in the feature
// panels, release details from GitHub, and a cookieless page-view count.
(() => {
  "use strict";
  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const $ = (selector, root = document) => root.querySelector(selector);
  const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];
  // The pause button stops every demo; sleeps wait while paused, so the
  // scripted demos stop where they are and carry on when resumed.
  let paused = false;
  try { paused = localStorage.getItem("quintess-paused") === "1"; } catch {}
  const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const sleep = async (ms) => {
    await wait(ms);
    while (paused) await wait(200);
  };
  const pauseButton = $("[data-pause]");
  const showPaused = () => {
    document.body.classList.toggle("paused", paused);
    if (pauseButton) {
      pauseButton.setAttribute("aria-pressed", String(paused));
      pauseButton.textContent = paused ? "play animation" : "pause animation";
    }
  };
  pauseButton?.addEventListener("click", () => {
    paused = !paused;
    try { localStorage.setItem("quintess-paused", paused ? "1" : "0"); } catch {}
    showPaused();
  });
  showPaused();

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

  // ---- Reveal sections and start demos when they come into view ----
  const whenVisible = (element, callback, threshold = 0.25) => {
    if (!("IntersectionObserver" in window)) return callback(true);
    new IntersectionObserver((entries) => entries.forEach((entry) => callback(entry.isIntersecting)), { threshold }).observe(element);
  };
  $$(".sec-head, .bento .cell, .private > *, .versus, .stat, .faq, .download-card").forEach((element) => {
    element.classList.add("reveal");
    whenVisible(element, (visible) => visible && element.classList.add("in"), 0.12);
  });

  // ---- Ticker: repeat the track so the loop has no seam ----
  const track_ = $(".ticker-track");
  if (track_) track_.innerHTML += track_.innerHTML;

  // ---- The hero demo ----
  const stage = $("[data-stage]");
  if (stage) heroDemo(stage);

  function heroDemo(stage) {
    const overlay = $("[data-overlay]", stage);
    const cursor = $("[data-cursor]", stage);
    const editBox = $("[data-edit-box]", stage);
    const scanLine = $("[data-scan-line]", stage);
    const note = $("[data-note]", stage);
    const editTarget = $("[data-edit-target]", stage);
    const reflow = $("[data-reflow]", stage);
    const redactTarget = $("[data-redact-target]", stage);
    const signature = $(".signature", stage);
    const seal = $("[data-seal]", stage);
    const qr = $("[data-qr]", stage);
    const steps = $$("[data-step]", stage);
    const tools = $$("[data-tool]", stage);
    const originalFee = editTarget.textContent;

    // A box for an element, relative to the overlay.
    const box = (element) => {
      const outer = overlay.getBoundingClientRect();
      const inner = element.getBoundingClientRect();
      return { left: inner.left - outer.left, top: inner.top - outer.top, width: inner.width, height: inner.height };
    };
    const place = (element, rect, pad = 0) => {
      Object.assign(element.style, { left: rect.left - pad + "px", top: rect.top - pad + "px", width: rect.width + pad * 2 + "px", height: rect.height + pad * 2 + "px" });
    };
    const moveCursor = (x, y) => {
      cursor.style.setProperty("--cx", x + "px");
      cursor.style.setProperty("--cy", y + "px");
    };
    const showNote = (html, x, y) => {
      note.innerHTML = html;
      const width = overlay.clientWidth;
      note.style.left = Math.max(4, Math.min(x, width - note.offsetWidth - 4)) + "px";
      note.style.top = y + "px";
      note.classList.add("on");
    };
    const setStep = (name) => {
      let reached = false;
      steps.forEach((step) => {
        const current = step.dataset.step === name;
        step.classList.toggle("active", current);
        step.classList.toggle("done", !reached && !current && name !== null);
        if (current) reached = true;
      });
      tools.forEach((tool) => tool.classList.toggle("active", tool.dataset.tool === (name ?? "select")));
    };

    function reset() {
      editTarget.textContent = originalFee;
      editTarget.classList.remove("selected");
      redactTarget.classList.remove("gone", "marking", "applied");
      [editBox, note].forEach((element) => element.classList.remove("on", "moved"));
      scanLine.style.opacity = 0;
      signature.classList.remove("drawn");
      seal.classList.remove("on");
      steps.forEach((step) => step.classList.remove("active", "done"));
      setStep(null);
    }

    // The finished state, for people who prefer less motion.
    function finalState() {
      editTarget.textContent = "$3,850";
      redactTarget.classList.add("gone", "applied");
      signature.classList.add("drawn");
      seal.classList.add("on");
      steps.forEach((step) => step.classList.add("done"));
      moveCursor(-40, -40);
    }

    async function run() {
      reset();
      const area = overlay.getBoundingClientRect();
      moveCursor(area.width * 0.7, area.height * 1.02);
      await sleep(900);

      // 1. Edit text: retype the fee; the paragraph re-wraps by itself.
      setStep("edit");
      let target = box(editTarget);
      moveCursor(target.left + target.width * 0.6, target.top + target.height * 0.55);
      await sleep(1000);
      place(editBox, box(reflow), 6);
      editBox.classList.add("on");
      await sleep(400);
      editTarget.classList.add("selected");
      await sleep(500);
      editTarget.classList.remove("selected");
      editTarget.textContent = "";
      for (const letter of "$3,850 (fixed for two years)") {
        editTarget.textContent += letter;
        place(editBox, box(reflow), 6);
        await sleep(55);
      }
      editBox.classList.add("moved");
      target = box(reflow);
      showNote("<b>preview</b> · 2 lines re-wrapped, same font", target.left, target.top + target.height + 10);
      await sleep(1900);
      note.classList.remove("on");
      editBox.classList.remove("on", "moved");

      // 2. Redact the account number: drag a box, apply, the text is gone.
      setStep("redact");
      // The mark is drawn by the text's own element, so it stays on the
      // text if the page re-wraps.
      target = box(redactTarget);
      moveCursor(target.left - 2, target.top + 2);
      await sleep(1000);
      redactTarget.classList.add("marking");
      moveCursor(target.left + target.width + 2, target.top + target.height);
      await sleep(1000);
      redactTarget.classList.replace("marking", "applied");
      redactTarget.classList.add("gone");
      showNote("<b>removed</b> from the file, not just covered", target.left - 40, target.top + target.height + 10);
      await sleep(2000);
      note.classList.remove("on");

      // 3. Sign.
      setStep("sign");
      target = box(signature);
      moveCursor(target.left + 10, target.top + target.height * 0.7);
      await sleep(900);
      signature.classList.add("drawn");
      moveCursor(target.left + target.width * 0.95, target.top + target.height * 0.5);
      await sleep(1900);
      seal.classList.add("on");
      await sleep(1300);

      // 4. Read the QR code.
      setStep("qr");
      target = box(qr);
      moveCursor(target.left + target.width * 0.5, target.top + target.height * 0.5);
      await sleep(900);
      scanLine.style.transition = "none";
      Object.assign(scanLine.style, { left: target.left - 4 + "px", width: target.width + 8 + "px", top: target.top + "px", opacity: 1 });
      await sleep(30);
      scanLine.style.transition = "top 0.9s ease-in-out";
      scanLine.style.top = target.top + target.height + "px";
      await sleep(950);
      scanLine.style.opacity = 0;
      showNote("<b>destination</b> · https://quintess.cc · review before opening", target.left + target.width - 330, target.top - 38);
      await sleep(2400);
      note.classList.remove("on");
      steps.forEach((step) => step.classList.replace("active", "done"));
      await sleep(1200);
    }

    if (reducedMotion) {
      requestAnimationFrame(finalState);
      return;
    }
    let visible = true;
    whenVisible(stage, (now) => (visible = now), 0.1);
    (async function loop() {
      await sleep(400);
      for (;;) {
        // Wait while the demo is off screen or the tab is hidden.
        while (!visible || document.hidden) await sleep(500);
        await run();
      }
    })();
  }

  // ---- Feature panels ----
  const demos = {
    reflow(cell) {
      const swaps = $$(".swap", cell);
      let round = 0;
      return async () => {
        round++;
        for (const swap of swaps) {
          const options = swap.dataset.swap.split("|");
          const next = options[round % options.length];
          swap.classList.add("typing");
          while (swap.textContent.length) { swap.textContent = swap.textContent.slice(0, -1); await sleep(22); }
          for (const letter of next) { swap.textContent += letter; await sleep(45); }
          swap.classList.remove("typing");
          await sleep(500);
        }
        await sleep(1600);
      };
    },
    redact(cell) {
      const items = $$(".checks li", cell);
      const result = $(".check-result", cell);
      return async () => {
        items.forEach((item) => item.classList.remove("ok", "scan"));
        result.classList.remove("on");
        await sleep(500);
        for (const item of items) {
          item.classList.add("scan");
          await sleep(360);
          item.classList.replace("scan", "ok");
        }
        result.classList.add("on");
        await sleep(3200);
      };
    },
    palette(cell) {
      const query = $("[data-palette-query]", cell);
      const list = $("[data-palette-list]", cell);
      const commands = [
        ["Redact sensitive data", "tools"], ["Redact area", "tools"], ["Check for leftover text", "tools"], ["Rotate pages", "pages"],
        ["Reduce file size", "file"], ["Recognize text (OCR)", "tools"], ["Compare two files", "tools"], ["Combine files", "file"],
        ["Fill forms from a spreadsheet", "forms"], ["Sign document", "protect"], ["Split view", "view"], ["Search a folder", "edit"],
        ["Bates numbering", "pages"], ["Convert to PDF/A", "file"], ["Record an action", "tools"], ["Add header and footer", "pages"],
      ];
      const queries = ["red", "comb", "sign", "ocr", "bates"];
      let index = 0;
      const show = (text) => {
        const matches = commands.filter(([name]) => name.toLowerCase().includes(text.toLowerCase())).slice(0, 4);
        list.innerHTML = matches.map(([name, group]) => `<li>${name}<small>${group}</small></li>`).join("");
      };
      show("");
      list.innerHTML = commands.slice(0, 4).map(([name, group]) => `<li>${name}<small>${group}</small></li>`).join("");
      return async () => {
        const text = queries[index++ % queries.length];
        query.textContent = "";
        for (const letter of text) { query.textContent += letter; show(query.textContent); await sleep(140); }
        await sleep(1800);
        while (query.textContent.length) { query.textContent = query.textContent.slice(0, -1); await sleep(40); }
        list.innerHTML = commands.slice(0, 4).map(([name, group]) => `<li>${name}<small>${group}</small></li>`).join("");
        await sleep(500);
      };
    },
    scripts(cell) {
      const sample = $("[data-script]", cell);
      const texts = [["नमस्ते दुनिया", "ltr"], ["مرحبا بالعالم", "rtl"], ["שלום עולם", "rtl"], ["வணக்கம் உலகம்", "ltr"], ["Здравствуй, мир", "ltr"], ["こんにちは世界", "ltr"]];
      let index = 1;
      return async () => {
        await sleep(1800);
        sample.classList.add("out");
        await sleep(350);
        const [text, direction] = texts[index++ % texts.length];
        sample.textContent = text;
        sample.dir = direction;
        sample.classList.remove("out");
      };
    },
    versions(cell) {
      const [, second, third] = $$(".version", cell);
      return async () => {
        second.classList.remove("in"); third.classList.remove("in");
        await sleep(900);
        second.classList.add("in");
        await sleep(1100);
        third.classList.add("in");
        await sleep(2600);
      };
    },
    flow(cell) {
      const flow = $(".flow", cell);
      const steps = $$(".flow-step", cell);
      flow.classList.add("running");
      let index = 0;
      return async () => {
        steps.forEach((step, i) => step.classList.toggle("hit", i === index % steps.length));
        index++;
        await sleep(1000);
      };
    },
    terminal(cell) {
      const output = $("[data-terminal]", cell);
      const script = [
        ["cmd", "quintess redact contract.pdf --pattern ssn --report"],
        ["out", "removed 3 matches on pages 2, 5"],
        ["ok", "check: no matching text found"],
        ["cmd", "quintess bates *.pdf --prefix ACME-"],
        ["ok", "numbered 412 pages in 9 files"],
        ["cmd", "quintess pdfa scan.pdf --convert"],
        ["ok", "converted toward PDF/A-2b"],
      ];
      return async () => {
        output.innerHTML = "";
        for (const [kind, text] of script) {
          const line = document.createElement("div");
          output.append(line);
          if (kind === "cmd") {
            line.innerHTML = '<span class="prompt">&gt; </span><span class="cmd"></span>';
            const span = $(".cmd", line);
            for (const letter of text) { span.textContent += letter; await sleep(24); }
            await sleep(300);
          } else {
            line.className = kind === "ok" ? "ok" : "";
            line.textContent = (kind === "ok" ? "✓ " : "  ") + text;
            await sleep(450);
          }
          while (output.children.length > 7) output.firstChild.remove();
        }
        await sleep(2400);
      };
    },
  };

  $$("[data-demo]").forEach((cell) => {
    const make = demos[cell.dataset.demo];
    if (!make) return;
    const step = make(cell);
    if (reducedMotion) return;
    let visible = false;
    let running = false;
    whenVisible(cell, async (now) => {
      visible = now;
      if (!now || running) return;
      running = true;
      while (visible && !document.hidden) await step();
      running = false;
    }, 0.3);
  });

  // The QR panel shows the same code as the demo page.
  const qrDemo = $("[data-qr-demo]");
  const pageQr = $("[data-qr]");
  if (qrDemo && pageQr) qrDemo.prepend(pageQr.cloneNode(true));

  // ---- Counting numbers ----
  $$("[data-count]").forEach((element) => {
    const end = Number(element.dataset.count);
    whenVisible(element, (visible) => {
      if (!visible || element.dataset.counted) return;
      element.dataset.counted = "1";
      if (reducedMotion) { element.textContent = end; return; }
      const start = performance.now();
      const tick = (now) => {
        const t = Math.min(1, (now - start) / 1400);
        element.textContent = Math.round(end * (1 - Math.pow(1 - t, 3)));
        if (t < 1) requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    }, 0.5);
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
  const versus = $(".versus");
  if (versus) whenVisible(versus, (visible) => visible && versus.classList.add("seen"));

  // ---- Release details from GitHub (through this site, cached) ----
  fetch("/api/release").then((response) => (response.ok ? response.json() : null)).then((release) => {
    if (!release || !release.installer) return;
    const megabytes = (release.installer.size / 1048576).toFixed(1) + " MB";
    $$("[data-download]").forEach((link) => (link.href = release.installer.url));
    $$("[data-version]").forEach((element) => (element.textContent = release.version));
    const set = (selector, text) => $$(selector).forEach((element) => (element.textContent = text));
    set("[data-asset-name]", release.installer.name);
    set("[data-asset-size]", megabytes);
    set("[data-asset-date]", new Date(release.published).toLocaleDateString("en-US", { year: "numeric", month: "short", day: "numeric" }));
    if (release.sha256) set("[data-asset-sha]", release.sha256);
    set("[data-asset-line]", `${release.installer.name} · ${megabytes} · installs per user, no admin rights`);
  }).catch(() => {});
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
