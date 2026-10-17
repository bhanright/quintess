# Quintess parallelism plan

Status: proposed implementation plan, October 13, 2026. No performance gains have been measured yet.

## Recommended approach

Keep PDFium and introduce a small pool of worker processes for genuine parallel PDF rendering. First improve request scheduling and measure the current pipeline. Use ordinary threads for work that does not call PDFium. Keep document mutation under one owner, so background results cannot overwrite newer edits.

Adding renderer threads inside the existing process would not provide parallel PDFium execution: pdfium-render 0.9.4 enables `thread_safe` by default, and its mutex serializes calls. Do not disable that protection or share PDFium handles between threads/processes.

## Findings from the current implementation

- `crates/quintess-app/src/render.rs`: a single thread owns PDFium documents and processes a vector of render requests. Channels and the request queue are unbounded. Visible pages outrank previews and prefetch, but an already-running render cannot be preempted by a newer request.
- `crates/quintess-app/src/viewer.rs`: texture/pending keys use page and resolution. A pending prefetch request can suppress a new request at the same resolution when that page becomes visible; priority promotion needs its own path.
- `crates/quintess-app/src/app.rs`: completed images are rejected if document version or night mode differs. Preserve this protection and extend it with request IDs/generations.
- `crates/quintess-app/src/jobs.rs`: each job gets its own thread, without a shared concurrency budget. Those jobs can still contend on PDFium's mutex.
- `crates/quintess-app/src/viewer.rs`: paragraph previews render through their own thread. Include this path in the rendering service migration.
- `crates/quintess-app/src/tab.rs`: edits serialize a new document snapshot. The renderer then loads that snapshot again. Additional workers can amplify this cost if snapshots are broadcast eagerly.
- `crates/quintess-core/src/ocr.rs`: OCR currently renders, converts, recognizes and writes each page sequentially. Recognition also uses a shared OCR-engine mutex.

## Phase 1: establish a baseline and improve scheduling

Instrument request creation, queue wait, snapshot transfer/load, PDFium rendering, image conversion and UI texture upload separately. Measure time to first visible page, 95th-percentile visible-page latency, frame stalls, batch throughput, and combined parent/child peak memory. Record canceled work and cache hit rate. Keep diagnostics local and avoid document text, filenames and passwords in logs.

Use text-heavy, scanned, vector-heavy, transparency-heavy, encrypted and malformed PDFs. Include scrolling in both directions, rapid zoom, split views, multiple tabs, editing during rendering, and viewing while OCR/export runs. Compare identical files, DPI, cache settings and build configuration; separate cold and warm runs.

Replace the vector scheduler with bounded, deduplicated pending work. Use a canonical render key containing tab, document version, page, normalized resolution and appearance settings. Distinguish queued and in-flight work; promote an existing queued request when its page becomes visible rather than duplicating it. Maintain an authoritative latest document generation even when no new render is queued.

Provide explicit document-update, tab-close and viewport-update messages. Cancel queued work for obsolete versions and distant pages. Every request must finish with success, failure or cancellation so pending state can be released without touching a newer request with the same page/DPI.

Reserve capacity for visible work, but allow background work to advance when the viewer is idle. Use bounded quotas/time-based aging for previews; do not let speculative prefetch fill every worker. Keep at most one assigned PDFium operation per worker, with the central scheduler retaining the backlog.

Deliverable: measured baseline and a responsive scheduler using the existing single renderer. Validate priority promotion, bounded queues, cancellation, tab cleanup and unchanged render output before adding processes.

## Phase 2: separate rendering from the app

Introduce a rendering-service interface returning owned pixels and metadata rather than `PdfDocument` handles. Preserve the in-process backend for rollback and comparison.

Prototype one child process using the existing Quintess executable in a private worker mode. Use a framed/versioned protocol over inherited local pipes with messages for handshake, load snapshot, render, cancel queued work, close document and shutdown. Keep logging on stderr, separate from protocol output. Pass passwords through the private pipe, never command-line arguments or logs.

Each worker owns its own PDFium instance and handles. Load an immutable document snapshot once per worker/generation, then send small page requests referencing it. Transfer only to workers that need the document. Return bounded raw pixel buffers with validated dimensions/stride/length, request ID and generation. Avoid PNG encoding in the interactive path unless measurement shows it helps.

Initially copy buffers through pipes. Add shared memory/read-only mappings only if transfer cost is material; introducing them requires explicit ownership, platform-specific permissions and cleanup. A worker process provides crash separation but is not automatically a security sandbox.

Test child startup, missing PDFium, encrypted documents, protocol mismatch, malformed responses, EOF, crash and orderly shutdown on Windows x64/ARM64, macOS and Linux. Recover pending requests after failure with bounded retries; quarantine a repeatedly failing request rather than looping. Avoid switching backend silently while an unsafe/mutating job is running.

Deliverable: one child worker behind a development setting, with render fidelity and overhead compared to the baseline.

## Phase 3: add a small adaptive pool

Start with two worker processes: one available for foreground work and one for previews/prefetch or background rendering. Start the second lazily after the first visible page, so cold opening does not pay for the whole pool. Test three or four workers for throughput workloads; do not default to one worker per logical CPU.

The scheduler keeps per-worker document affinity and load state to avoid repeated parsing, but may replicate an active snapshot across workers when multiple pages need rendering. Bound resident documents and use LRU eviction; close tab snapshots immediately when no request uses them. Expire old generations after in-flight results complete.

Enforce one combined memory policy for textures, queued/in-flight pixels, source snapshots, worker document caches and process overhead. Reconcile the existing page-cache setting with total-process accounting; distinguish estimated reservations from measured resident memory. Reduce prefetch and worker count before exceeding the budget. Include source duplication and PDFium's internal caches in measurements.

Suppress late results after edits, zoom/appearance changes, tab closure and worker restart. Cancel queued jobs immediately. For an already-running render, discard obsolete output initially; consider PDFium's progressive-render APIs only after confirming support in the pinned bindings. Terminate a stuck worker only after a deadline, clearing its affected requests and limiting retries.

Deliverable: two workers improve foreground responsiveness or throughput while meeting agreed memory/startup limits. Keep one-worker mode for small machines or regressions.

## Phase 4: move background PDFium workloads out of the UI process

Route paragraph preview rendering and PDFium portions of OCR/export/thumbnail jobs through the rendering service. Give interactive page renders higher priority and reserve foreground capacity during long operations. Large operations must supply page requests incrementally, with backpressure and cancellation between pages.

For text extraction/search, first measure UI-process PDFium contention. If material, extend the worker protocol to return owned text/geometry and make affected UI operations asynchronous. This is a separate migration because the app currently holds PDFium document handles for positioned text and fallback metadata. Raster workers alone do not remove every UI-process lock wait.

Use bounded thread pools for image conversion, night-mode transforms, compression and other independent CPU work. Budget OCR inference's internal threads together with worker-process CPU use; avoid multiplying pools across every page. Do not assume the OCR engine becomes parallel by adding callers to its existing mutex.

For OCR, pipeline render -> recognize -> apply recognized text. Keep only a small number of page images in flight. Preserve page order, and apply document changes through a single owner. For batch operations across unrelated documents, use independent document jobs under the same CPU/memory limits.

All document-changing job outcomes must carry their input document version. If the user edits the document while a job runs, reject or explicitly reconcile the old result; never silently overwrite the new document. Build a complete successful output before exposing it as a saved result.

## Validation and rollout

Suggested initial acceptance gates, to be revised after the baseline:

- Identical output with the same PDFium build/settings; deterministic and visual comparisons across the fixture corpus.
- No lost requests, stale textures, overwritten edits, orphan workers or leaked document snapshots.
- At least 25% lower 95th-percentile visible-page latency during concurrent background work, or at least 1.5x page-render throughput on the target multicore PC. These are targets, not predictions.
- No more than 10% cold first-page regression; enforce the agreed combined memory envelope.
- Worker failure leaves the app usable and retries are bounded.
- Tests cover rapid scrolling, priority promotion, simultaneous panes/tabs, edit/close races, password-protected files, low memory and crash recovery.
- Cross-platform packaging and same-executable worker mode retain normal GUI/CLI behavior.

Ship in separate reviewable changes: instrumentation/scheduling; one-worker protocol; adaptive pool; background-work migration. Compare one and two workers first. Keep the new backend opt-in until fidelity, resource and recovery gates pass; then enable conservative defaults with a fallback setting.

## Sources

- [pdfium-render threading documentation](https://github.com/ajrcarey/pdfium-render#multi-threading): calls are mutex-serialized; process-based parallelism is recommended for simultaneous PDFium work.
- [PDFium public API](https://pdfium.googlesource.com/pdfium/+/refs/heads/main/public/fpdfview.h): rendering and document-loading interfaces.
- Current local implementation: `render.rs`, `viewer.rs`, `tab.rs`, `jobs.rs`, `app.rs`, `engine.rs` and `ocr.rs`.
