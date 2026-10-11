"""
Makes the website's short recording of a real edit in Quintess: the app is
started once per frame with its self-check screenshot mode, at each step of
retyping a fee in a paragraph, and ffmpeg joins the pictures into a clip.

    python site/tools/make_recording.py

Needs a built app (target/debug or target/release), ffmpeg on the PATH, and
the sample agreement (made here with the CLI). Writes
site/public/assets/video/edit.mp4 and edit.webm, and a poster picture.
"""
import os
import pathlib
import shutil
import subprocess
import sys

SITE = pathlib.Path(__file__).resolve().parents[1]
REPOSITORY = SITE.parent
WORK = REPOSITORY / "scratch" / "recording"
OUTPUT = SITE / "public" / "assets" / "video"

# The paragraph as it starts, and the words typed in place of the fee.
BEFORE = "The quarterly fee is $4,200, payable within thirty days of each delivery. Late payments carry a charge of one percent per month on the unpaid amount."
OLD_FEE = "$4,200"
NEW_WORDS = "$3,850 for the first two years"

AGREEMENT = """# Supply Agreement

**Northwind Supply Co.** and **Harbor Street Books** agree to the following terms, effective October 1, 2026.

## 1. Delivery

The Supplier will deliver 480 units of archival paper stock to the Buyer each quarter. Each delivery includes freight to the Buyer's main warehouse and a packing list signed by the driver.

## 2. Fees

The quarterly fee is $4,200, payable within thirty days of each delivery. Late payments carry a charge of one percent per month on the unpaid amount.

## 3. Ending the agreement

Either party may end this agreement with sixty days' written notice. Payments go to the account the Supplier names in writing.

## 4. Signatures

Signed for the Supplier and for the Buyer on the dates written below.
"""


def program(name: str) -> pathlib.Path:
    for build in ("release", "debug"):
        path = REPOSITORY / "target" / build / f"{name}.exe"
        if path.exists():
            return path
    sys.exit(f"build {name} first")


def frame(setup: str, path: pathlib.Path, document: pathlib.Path) -> None:
    environment = dict(os.environ, QUINTESS_SCREENSHOT=str(path), QUINTESS_SETUP=setup)
    subprocess.run([str(program("quintess-app")), str(document)], env=environment, check=True, timeout=120)


def main() -> None:
    if WORK.exists():
        shutil.rmtree(WORK)
    WORK.mkdir(parents=True)
    OUTPUT.mkdir(parents=True, exist_ok=True)
    markdown = WORK / "agreement.md"
    markdown.write_text(AGREEMENT, encoding="utf-8")
    document = WORK / "supply-agreement.pdf"
    subprocess.run([str(program("quintess")), "create", "markdown", str(markdown), "-o", str(document)], check=True)

    # (setup, seconds on screen). Commas in typed text are written as ";"
    # because the setup list itself is comma-separated.
    steps: list[tuple[str, float]] = [("dark", 1.2), ("dark,tool=edit-text", 0.6), ("dark,edit-paragraph=6,wait=1", 1.0)]
    start = BEFORE.index(OLD_FEE)
    prefix, suffix = BEFORE[:start], BEFORE[start + len(OLD_FEE):]
    for keep in range(len(OLD_FEE) - 1, -1, -1):
        text = prefix + OLD_FEE[:keep] + suffix
        steps.append((f"dark,edit-paragraph=6,edit-text={text.replace(',', ';')}", 0.12))
    words = NEW_WORDS.split(" ")
    for count in range(1, len(words) + 1):
        text = prefix + " ".join(words[:count]) + suffix
        steps.append((f"dark,edit-paragraph=6,edit-text={text.replace(',', ';')}", 0.35))
    final = (prefix + NEW_WORDS + suffix).replace(",", ";")
    steps.append((f"dark,edit-paragraph=6,edit-text={final},wait=4", 2.2))
    steps.append((f"dark,edit-paragraph=6,edit-text={final},show-result,wait=5", 3.0))

    listing = []
    for index, (setup, seconds) in enumerate(steps):
        picture = WORK / f"frame-{index:03}.png"
        print(f"frame {index + 1} of {len(steps)}")
        frame(setup, picture, document)
        listing.append(f"file '{picture.as_posix()}'\nduration {seconds}")
    # ffmpeg's concat list needs the last picture repeated to hold its time.
    listing.append(f"file '{(WORK / f'frame-{len(steps) - 1:03}.png').as_posix()}'")
    (WORK / "frames.txt").write_text("\n".join(listing), encoding="utf-8")

    # Crop to the page and the editing toolbar, leaving some app chrome.
    crop = "crop=1040:700:240:40"
    common = ["ffmpeg", "-y", "-f", "concat", "-safe", "0", "-i", str(WORK / "frames.txt"), "-vf", f"{crop},fps=20,format=yuv420p"]
    subprocess.run(common + ["-c:v", "libx264", "-crf", "26", "-movflags", "+faststart", str(OUTPUT / "edit.mp4")], check=True)
    subprocess.run(common + ["-c:v", "libvpx-vp9", "-crf", "36", "-b:v", "0", str(OUTPUT / "edit.webm")], check=True)
    subprocess.run(["ffmpeg", "-y", "-i", str(WORK / "frame-002.png"), "-vf", crop, "-frames:v", "1", str(OUTPUT / "edit-poster.webp")], check=True)
    for name in ("edit.mp4", "edit.webm", "edit-poster.webp"):
        print(name, (OUTPUT / name).stat().st_size // 1024, "KB")


if __name__ == "__main__":
    main()
