"""
Builds the macOS release of Quintess: one universal program (Apple silicon
and Intel, joined with lipo) in an app bundle, on a disk image.

    installer/output/Quintess-<version>-macos.dmg   (and its .sha256)

    python3 tools/package_macos.py [--skip-build] [--pdfium DIR] [--models DIR] [--output DIR]

Run it on a Mac with Rust and both targets installed
(`rustup target add aarch64-apple-darwin x86_64-apple-darwin`) and Xcode's
command-line tools (lipo, codesign, hdiutil). .github/workflows/macos-release.yml
runs it on GitHub's macOS runners. The bundle:

    Quintess.app/Contents/Info.plist          from installer/macos/Info.plist.in
    Quintess.app/Contents/MacOS/quintess      the app, and with a command the command-line tool
    Quintess.app/Contents/Frameworks/libpdfium.dylib
    Quintess.app/Contents/Resources/quintess.icns, dictionaries/, models/, licenses/

The program finds PDFium in Frameworks and its data in Resources through
quintess_core::system (libraries_folder, resources_folder). PDFium is opened
by its full path at run time, so the program needs no rpath for it.

Signing: with MACOS_SIGNING_IDENTITY set (a Developer ID Application
identity; installer/macos/import-signing-certificate.sh sets it in CI), the
bundle is signed with the hardened runtime and a secure timestamp, and
notarized when APPLE_ID, APPLE_PASSWORD (an app-specific password) and
APPLE_TEAM_ID are set too. Without an identity it is ad-hoc signed
(`codesign -s -`), which Apple silicon requires of every program but which
Gatekeeper doesn't trust: people open it the first time as docs/USER_GUIDE.md
explains.

The signing order, the disk image retries and the check of the app inside
the image are adapted from PdfCraft's packaging/macos/package.sh
(https://github.com/storytold/pdfcraft, commit 6d843a8e8b22e4442666eb9e12f506c40458af3d),
Copyright (c) 2026 ArtCraft Team and the PdfCraft contributors, MIT OR
Apache-2.0. Modified: rewritten in Python for Quintess's bundle layout,
with PDFium in Frameworks, no separate command-line archive and no custom
disk image background.
"""

import argparse
import hashlib
import os
import pathlib
import shutil
import subprocess
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
INSTALLER = ROOT / "installer" / "macos"
DICTIONARIES = ROOT / "assets" / "dictionaries"
ICON = ROOT / "assets" / "macos" / "quintess.icns"
TARGETS = ["aarch64-apple-darwin", "x86_64-apple-darwin"]
# The oldest macOS the program runs on; must match LSMinimumSystemVersion
# in installer/macos/Info.plist.in. Big Sur is the first for Apple silicon.
MACOS_DEPLOYMENT_TARGET = "11.0"

# The OCR models, checked against the checksums the program itself demands
# (crates/quintess-core/src/ocr.rs).
MODELS = {
    "text-detection.onnx": "a917b23dbd9524b465df7e922641b3ff2981623df4ded5a0234004ef2fee7cfe",
    "text-recognition.onnx": "86c145c2edb96c8caed5b1ebb8f44d706408922211451c309c625157dd6061c5",
}

FIRST_OPEN_TEXT = """Quintess {version} for macOS (Apple silicon and Intel)

Install: drag Quintess into Applications.

Quintess is not signed with an Apple Developer ID yet, so the first time
macOS says it can't check it for malicious software. To open it anyway:

  macOS 15 (Sequoia) and later: open Quintess once (it is refused), then go
  to System Settings > Privacy & Security, scroll to Security and click
  "Open Anyway" beside the message about Quintess, and confirm.

  macOS 14 (Sonoma) and earlier: Control-click (or right-click) Quintess in
  Applications, choose Open, then Open again.

If macOS says Quintess "is damaged", the download was marked as quarantined;
in Terminal:
    xattr -dr com.apple.quarantine /Applications/Quintess.app

After that it opens normally. PDFs can be opened with it from the Finder
(Open With, or make it the default in a PDF's Get Info window).

The command-line tool is the same program:
    /Applications/Quintess.app/Contents/MacOS/quintess --help
To type just `quintess`, link it once:
    sudo ln -sf /Applications/Quintess.app/Contents/MacOS/quintess /usr/local/bin/quintess

The user guide: https://quintess.cc/guide/ (also F1 in the app).
Licenses: Quintess.app/Contents/Resources/licenses.
"""


def run(*command, **options):
    print("+", " ".join(str(part) for part in command), flush=True)
    return subprocess.run([str(part) for part in command], check=True, **options)


def workspace_version():
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    section = text.split("[workspace.package]", 1)[1].split("\n[", 1)[0]
    for line in section.splitlines():
        if line.strip().startswith("version"):
            return line.split("=", 1)[1].strip().strip('"')
    raise SystemExit("no version in Cargo.toml's [workspace.package]")


def sha256_of(path):
    digest = hashlib.sha256()
    with open(path, "rb") as file:
        for block in iter(lambda: file.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def target_folder():
    return pathlib.Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))


def build():
    environment = dict(os.environ, MACOSX_DEPLOYMENT_TARGET=MACOS_DEPLOYMENT_TARGET)
    command = ["cargo", "build", "--release", "-j", "4", "-p", "quintess-cli"]
    for target in TARGETS:
        command += ["--target", target]
    run(*command, cwd=ROOT, env=environment)


def universal_program(work):
    """Joins the two builds into one program that runs on either processor."""
    program = work / "quintess"
    run("lipo", "-create", "-output", program, *[target_folder() / target / "release" / "quintess" for target in TARGETS])
    run("lipo", "-info", program)
    return program


def commit():
    try:
        return subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=ROOT, capture_output=True, text=True, check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return "unknown"


def copy_folder_files(source, destination, pattern):
    destination.mkdir(parents=True, exist_ok=True)
    for path in sorted(source.glob(pattern)):
        if path.is_file():
            shutil.copy2(path, destination / path.name)


def assemble_bundle(app, version, program, pdfium, models):
    if app.exists():
        shutil.rmtree(app)
    contents = app / "Contents"
    for folder in ["MacOS", "Frameworks", "Resources"]:
        (contents / folder).mkdir(parents=True)
    info = (INSTALLER / "Info.plist.in").read_text(encoding="utf-8").replace("@VERSION@", version).replace("@COMMIT@", commit())
    (contents / "Info.plist").write_text(info, encoding="utf-8")
    run("plutil", "-lint", contents / "Info.plist")
    (contents / "PkgInfo").write_bytes(b"APPL????")
    shutil.copy2(program, contents / "MacOS" / "quintess")
    shutil.copy2(pdfium / "lib" / "libpdfium.dylib", contents / "Frameworks" / "libpdfium.dylib")
    resources = contents / "Resources"
    shutil.copy2(ICON, resources / "quintess.icns")
    copy_folder_files(DICTIONARIES, resources / "dictionaries", "*.aff")
    copy_folder_files(DICTIONARIES, resources / "dictionaries", "*.dic")
    (resources / "models").mkdir()
    for name in MODELS:
        shutil.copy2(models / name, resources / "models" / name)
    licenses = resources / "licenses"
    copy_folder_files(DICTIONARIES, licenses / "dictionaries", "*.license.txt")
    for name in ["LICENSE-MIT", "LICENSE-APACHE"]:
        shutil.copy2(ROOT / name, licenses / name)
    shutil.copy2(ROOT / "licenses" / "THIRD-PARTY-NOTICES.txt", licenses / "THIRD-PARTY-NOTICES.txt")
    copy_folder_files(pdfium / "licenses", licenses / "pdfium", "*")
    shutil.copy2(pdfium / "LICENSE", licenses / "pdfium" / "pdfium-binaries-build-scripts.txt")


class Signer:
    def __init__(self):
        self.identity = os.environ.get("MACOS_SIGNING_IDENTITY", "").strip() or "-"
        self.keychain = os.environ.get("MACOS_KEYCHAIN", "").strip()
        self.notarize = self.identity != "-" and all(os.environ.get(name) for name in ["APPLE_ID", "APPLE_PASSWORD", "APPLE_TEAM_ID"])

    @property
    def ad_hoc(self):
        return self.identity == "-"

    def sign(self, path, hardened=False):
        command = ["codesign", "--force", "--sign", self.identity]
        if self.keychain:
            command += ["--keychain", self.keychain]
        # An ad-hoc signature can't be timestamped, and the hardened
        # runtime's library check would refuse an ad-hoc PDFium, so both
        # come only with a real identity.
        if self.ad_hoc:
            command.append("--timestamp=none")
        else:
            command.append("--timestamp")
            if hardened:
                command += ["--options", "runtime", "--entitlements", INSTALLER / "entitlements.plist"]
        run(*command, path)

    def submit_for_notarization(self, path, staple):
        """Sends `path` to Apple and staples the ticket to `staple` (an app
        is sent zipped, but the ticket belongs on the app itself)."""
        print(f"notarizing {path.name} (this can take a few minutes)", flush=True)
        credentials = ["--apple-id", os.environ["APPLE_ID"], "--password", os.environ["APPLE_PASSWORD"], "--team-id", os.environ["APPLE_TEAM_ID"]]
        result = subprocess.run(["xcrun", "notarytool", "submit", str(path), *credentials, "--wait", "--timeout", "1h"], capture_output=True, text=True)
        print(result.stdout, result.stderr)
        if result.returncode != 0 or "status: Accepted" not in result.stdout:
            raise SystemExit(f"notarization of {path.name} failed")
        run("xcrun", "stapler", "staple", staple)


def sign_bundle(signer, app):
    # Inside out: the library and the program first, then the bundle, which
    # seals Resources (no --deep on the bundle's own signature).
    contents = app / "Contents"
    signer.sign(contents / "Frameworks" / "libpdfium.dylib")
    signer.sign(contents / "MacOS" / "quintess", hardened=True)
    signer.sign(app, hardened=True)
    run("codesign", "--verify", "--strict", "--deep", "--verbose=2", app)


def hdiutil(what, *arguments):
    """hdiutil fails now and then on CI runners ("Resource busy"), so each
    step gets five tries with growing pauses."""
    for attempt in range(1, 6):
        if subprocess.run(["hdiutil", *[str(part) for part in arguments]]).returncode == 0:
            return
        if attempt == 5:
            raise SystemExit(f"hdiutil {what} failed five times")
        print(f"hdiutil {what} failed (attempt {attempt} of 5); trying again in {attempt * 10} s", flush=True)
        time.sleep(attempt * 10)


def make_disk_image(signer, app, work, image, version):
    stage = work / "disk image"
    if stage.exists():
        shutil.rmtree(stage)
    stage.mkdir()
    # ditto keeps the bundle's signature and extended attributes intact.
    run("ditto", app, stage / "Quintess.app")
    (stage / "Applications").symlink_to("/Applications")
    (stage / "Read me first.txt").write_text(FIRST_OPEN_TEXT.format(version=version), encoding="utf-8")
    if image.exists():
        image.unlink()
    # `create -srcfolder` copies the files as they are (`makehybrid` would
    # add Finder information that `codesign --verify --strict` rejects).
    hdiutil("create", "create", "-srcfolder", stage, "-volname", "Quintess", "-fs", "HFS+", "-format", "UDZO", "-imagekey", "zlib-level=9", "-ov", image)
    signer.sign(image)
    # The app inside the image must still pass the strict check.
    mount = work / "mounted"
    mount.mkdir(exist_ok=True)
    hdiutil("attach", "attach", "-nobrowse", "-readonly", "-mountpoint", mount, image)
    try:
        run("codesign", "--verify", "--strict", "--deep", "--verbose=2", mount / "Quintess.app")
    finally:
        hdiutil("detach", "detach", mount)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--skip-build", action="store_true", help="package the release programs already built for both targets")
    parser.add_argument("--pdfium", default=str(ROOT / "vendor" / "pdfium-mac"), help="unpacked pdfium-mac-univ.tgz (chromium/8086)")
    parser.add_argument("--models", default=str(ROOT / "vendor" / "ocrs-models"), help="folder with the two OCR models")
    parser.add_argument("--output", default=str(ROOT / "installer" / "output"), help="where the disk image is written")
    arguments = parser.parse_args()

    version = workspace_version()
    pdfium, models, output = pathlib.Path(arguments.pdfium), pathlib.Path(arguments.models), pathlib.Path(arguments.output)
    if not (pdfium / "lib" / "libpdfium.dylib").is_file():
        raise SystemExit(f"libpdfium.dylib not found in {pdfium}/lib; unpack pdfium-mac-univ.tgz from pdfium-binaries chromium/8086 there")
    for name, expected in MODELS.items():
        if not (models / name).is_file():
            raise SystemExit(f"{models / name} is missing; `quintess ocr` downloads the models once, or see docs/BUILDING.md")
        if sha256_of(models / name) != expected:
            raise SystemExit(f"{models / name} is not the expected model (its checksum differs)")
    for required in [ROOT / "licenses" / "THIRD-PARTY-NOTICES.txt", ICON]:
        if not required.is_file():
            raise SystemExit(f"{required} is missing (tools/third_party_notices.py and tools/make_icons.py write them)")

    if not arguments.skip_build:
        build()
    work = target_folder() / "macos-package"
    work.mkdir(parents=True, exist_ok=True)
    program = universal_program(work)
    # PDFium must run on both processors too.
    run("lipo", "-verify_arch", pdfium / "lib" / "libpdfium.dylib", "arm64", "x86_64")

    app = work / "Quintess.app"
    assemble_bundle(app, version, program, pdfium, models)
    signer = Signer()
    print(f"signing with {'an ad-hoc signature' if signer.ad_hoc else signer.identity}; notarizing: {signer.notarize}", flush=True)
    sign_bundle(signer, app)
    if signer.notarize:
        archive = work / "Quintess-notarize.zip"
        run("ditto", "-c", "-k", "--keepParent", app, archive)
        signer.submit_for_notarization(archive, staple=app)

    output.mkdir(parents=True, exist_ok=True)
    image = output / f"Quintess-{version}-macos.dmg"
    make_disk_image(signer, app, work, image, version)
    if signer.notarize:
        signer.submit_for_notarization(image, staple=image)
    checksum = image.with_name(image.name + ".sha256")
    checksum.write_text(f"{sha256_of(image)}  {image.name}\n", encoding="utf-8")
    print(f"wrote {image} ({image.stat().st_size / 1e6:.1f} MB)")


if __name__ == "__main__":
    main()
