"""
Makes the two documents the speed and memory comparison uses, so anyone
can rebuild them and get the same files:

- long.pdf: a 624-page text document (900 sections of six paragraphs),
  made from Markdown by Quintess.
- scans.pdf: a 150-page "scanned" document, the first 150 pages of
  long.pdf rendered at 200 dpi as JPEG pictures and put back into a PDF,
  as a scanner would make it (no text layer).

    python tools/benchmark/make_fixtures.py [output folder]

Needs a built `quintess` CLI (target/release or target/debug). Prints each
file's size and SHA-256.
"""
import hashlib
import pathlib
import shutil
import subprocess
import sys

REPOSITORY = pathlib.Path(__file__).resolve().parents[2]
CLAUSE = (
    "The Supplier will deliver archival paper stock to the Buyer each quarter, with freight included "
    "to the main warehouse and a packing list signed by the driver. Payment is due within thirty days "
    "of each delivery, and late payments carry a monthly charge on the unpaid amount."
)
SECTIONS = 900
PARAGRAPHS_PER_SECTION = 6
SCANNED_PAGES = 150
SCAN_DOTS_PER_INCH = 200


def cli() -> pathlib.Path:
    for build in ("release", "debug"):
        for name in ("quintess.exe", "quintess"):
            path = REPOSITORY / "target" / build / name
            if path.exists():
                return path
    sys.exit("build the quintess CLI first (cargo build --release -p quintess-cli)")


def long_markdown() -> str:
    sections = []
    for section in range(1, SECTIONS + 1):
        paragraphs = [f"This is paragraph {number} of section {section}. {CLAUSE}" for number in range(1, PARAGRAPHS_PER_SECTION + 1)]
        sections.append(f"# Section {section}\n" + "\n\n".join(paragraphs) + "\n")
    return "\n".join(sections) + "\n\n"


def sha256(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> None:
    output = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else REPOSITORY / "scratch" / "benchmark"
    output.mkdir(parents=True, exist_ok=True)
    quintess = str(cli())

    markdown = output / "long.md"
    markdown.write_text(long_markdown(), encoding="utf-8")
    long_pdf = output / "long.pdf"
    subprocess.run([quintess, "create", "markdown", str(markdown), "-o", str(long_pdf)], check=True)

    pictures = output / "scan-pages"
    if pictures.exists():
        shutil.rmtree(pictures)
    pictures.mkdir()
    subprocess.run([quintess, "render", str(long_pdf), "--pages", f"1-{SCANNED_PAGES}", "--dpi", str(SCAN_DOTS_PER_INCH), "--format", "jpeg", "-o", str(pictures)], check=True)
    pages = sorted(pictures.glob("*.jpg"))
    scans_pdf = output / "scans.pdf"
    subprocess.run([quintess, "create", "images", *map(str, pages), "-o", str(scans_pdf)], check=True)

    for path in (long_pdf, scans_pdf):
        print(f"{path.name}: {path.stat().st_size:,} bytes, SHA-256 {sha256(path)}")


if __name__ == "__main__":
    main()
