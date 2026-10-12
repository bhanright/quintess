# Robustness testing

How Quintess copes with real-world and broken PDFs is measured three ways:

- the **corpus sweep**: every file of the pdf.js test corpus opened,
  rendered, read, edited, saved and reopened, each in a process of its own;
- **cargo-fuzz targets** for Quintess's own parsing and saving;
- **mutation fuzzing** through the sweep's full check, PDFium included.

All three live outside the shipped program: the sweep and the mutation
fuzzer are the `quintess-robustness` tool (`tools/robustness`), the fuzz
targets are in `fuzz/`. The workflow `.github/workflows/robustness.yml`
runs the sweep on every push to main and nightly, and the fuzzers nightly.

The corpus fetch, the child-process runner, the baseline comparison and the
mutation fuzzer are adapted from PdfCraft (MIT OR Apache-2.0; see the
headers of those files and licenses/THIRD-PARTY-NOTICES.txt).

## The corpus

```
cargo run -p quintess-robustness -- fetch
```

fetches pdf.js's `test/pdfs` folder and `test/test_manifest.json` at the
commit pinned in `tools/robustness/src/corpus.rs` (a sparse Git checkout,
about 125 MB) and checks it against the recorded SHA-256 manifest digest,
then downloads the files its `.link` entries point to (each checked against
the MD5 in pdf.js's manifest; about 450 files from many sites, some long
gone). It goes into `corpus/` at the top of the repository (or `--corpus
DIR`, or `QUINTESS_CORPUS`), which Git ignores. Never commit corpus files:
they keep their own terms. `corpus/pdfjs-linked/fetch-report.json` lists the
linked files that couldn't be fetched. To move the pin, run
`fetch --update-pin`, paste the printed constants into `corpus.rs` and the
commit into the workflow's `PDFJS_COMMIT`, and record a new baseline.

Passwords for the encrypted test files come from pdf.js's manifest.

## The sweep

```
QUINTESS_PDFIUM_DIR=<folder with pdfium.dll or libpdfium.so> \
  cargo run -p quintess-robustness -- sweep [--jobs N] [--timeout 120] [--check]
```

For every file, a child process (`quintess-robustness check-file`) opens it
with lopdf (`Pdf::from_bytes`) and with PDFium, renders every page at 18
DPI, extracts each page's text, reads the comments, form fields, bookmarks
and properties, then makes an edit round trip: rotates page 1, adds a
comment and sets the title, saves through the command line's own save path
(`document::bytes_after_edit`, which keeps encryption and appends to signed
files as an incremental update), and reopens the result in both engines,
checking that every change is there and that PDFium sees the pages lopdf
wrote. Each stage announces itself on standard error, so a process that
dies is attributed to its file and stage.

Each file is classed as:

| Class | Meaning |
|---|---|
| ok | everything above worked |
| expected failure | encrypted with no known password, or broken on purpose so that pdf.js rejects it too (the list, with the pdf.js unit test that shows it, is `tools/robustness/src/pdfjs_rejects.rs`) |
| error | Quintess reported an error; its message is the reason |
| crash | the process died: a panic, an abort or a native crash in PDFium |
| hang | still running after the time limit |

Crashes and hangs are checked again alone, with twice the time, before
they count (a busy machine, or this PC's memory faults, can fake either);
one that doesn't happen again is marked flaky and takes the second run's
class. On Linux each child runs under a 4 GB address-space limit, so a
runaway allocation is a crash instead of a dead machine.

When `qpdf`, `pdfinfo` and `pdftotext` are installed (CI installs them),
each saved copy is checked with them too, and where they judge the copy
worse than the original (qpdf errors or warnings the original didn't have,
a different page count, text extraction failing) it is recorded as a
disagreement.

The results go to `robustness-out/sweep.json` (every file, with its
stages' findings) and `robustness-out/sweep.md` (counts and every failing
file with its reason).

### The baseline

`tools/robustness/baseline.json` records every file's class (and the
outside tools' disagreements). With `--check`, the sweep fails when a file
crashes or hangs that didn't before, when a file that was ok isn't, or when
the outside tools find a new disagreement; known failures don't fail it, and
linked files that couldn't be fetched this time are skipped. After a fix,
or once a change is understood, record a new one:

```
cargo run -p quintess-robustness -- sweep --update-baseline
```

The CI runner is Linux, so record the baseline from a CI run's
`sweep.json` when the classes differ between systems (they rarely do).

## Fuzzing

### cargo-fuzz (nightly, Linux or macOS)

```
cargo install cargo-fuzz
cd fuzz
cargo +nightly fuzz run parse -- -max_total_time=300
cargo +nightly fuzz run content_stream
cargo +nightly fuzz run save_round_trip
```

- `parse`: `Pdf::from_bytes`, then what opening reads (pages, page boxes and
  rotation, comments, fields, bookmarks, properties, page labels).
- `content_stream`: lopdf's content parser, its re-encoding (content that
  decoded must decode again) and `objects::list_objects` on a page whose
  content is the input.
- `save_round_trip`: the sweep's edit round trip through
  `document::bytes_after_edit`; a saved file Quintess can't open again is a
  finding.

Seed them with small corpus files: `mkdir -p corpus/parse && find
../corpus -name '*.pdf' -size -64k -exec cp {} corpus/parse/ \;` (inside
`fuzz/`; Git ignores `fuzz/corpus` and `fuzz/artifacts`). Crashes land in
`fuzz/artifacts/<target>/`; reproduce one with `cargo +nightly fuzz run
<target> <file>`. CI runs `--sanitizer none` (Quintess's code is safe Rust;
the address sanitizer would mostly slow the dependencies down).

### Mutation fuzzing (stable, any system)

```
QUINTESS_PDFIUM_DIR=... cargo run -p quintess-robustness -- mutate --time 300
```

mutates corpus files of 64 KB or less (and two synthetic files) and runs
each through `check-file` in a child process, so PDFium crashes are found
too. Crashes are minimized and saved with a note in
`robustness-out/mutate/findings/`.

Never commit a finding made from a corpus file: turn each bug into a
regression test with a small synthetic file.

## Results

See "Robustness" in docs/STATUS.md.
