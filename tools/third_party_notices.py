"""Writes licenses/THIRD-PARTY-NOTICES.txt: every library compiled into the
Quintess programs, with its license and the license text it ships, plus
PDFium and the OCR models. Run it before each release:

    python tools/third_party_notices.py

It reads `cargo metadata` for Windows and for Linux, follows only the
dependencies that end up in the programs (not test-only or build-only ones)
and copies each crate's own LICENSE / COPYING / NOTICE files. One file
covers both systems; crates built for only one are marked. Identical texts
are printed once and referred to afterwards.
"""

import hashlib
import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUTPUT = ROOT / "licenses" / "THIRD-PARTY-NOTICES.txt"
# The packages whose programs ship: quintess-cli builds `quintess` (which
# includes quintess-app) and quintess-console the Windows launcher, quintess.com.
PROGRAMS = ["quintess-cli", "quintess-console"]
PLATFORMS = {"x86_64-pc-windows-msvc": "Windows", "x86_64-unknown-linux-gnu": "Linux"}
LICENSE_FILE_PREFIXES = ("license", "licence", "copying", "notice", "copyright")


def shipped_packages(metadata):
    """Packages reachable from the programs through normal dependencies."""
    packages = {p["id"]: p for p in metadata["packages"]}
    nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
    workspace = set(metadata["workspace_members"])
    start = [pid for pid in workspace if packages[pid]["name"] in PROGRAMS]
    seen, stack = set(), list(start)
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes[pid]["deps"]:
            # Only dependencies used by the built program, not dev or build ones.
            if any(kind["kind"] is None for kind in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    return sorted((packages[pid] for pid in seen if pid not in workspace), key=lambda p: (p["name"].lower(), p["version"]))


def license_texts(folder):
    files = [f for f in sorted(folder.iterdir()) if f.is_file() and f.name.lower().startswith(LICENSE_FILE_PREFIXES)]
    return [(f.name, f.read_text(encoding="utf-8", errors="replace").strip()) for f in files]


def main():
    # Package id -> (package, the systems it is built for).
    found = {}
    for target, system in PLATFORMS.items():
        metadata = json.loads(subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--filter-platform", target],
            cwd=ROOT, capture_output=True, text=True, encoding="utf-8", check=True).stdout)
        for package in shipped_packages(metadata):
            found.setdefault(package["id"], (package, []))[1].append(system)
    packages = sorted((package for package, _ in found.values()), key=lambda p: (p["name"].lower(), p["version"]))
    only_on = {pid: systems[0] for pid, (_, systems) in found.items() if len(systems) == 1}
    lines = [
        "Third-party software in Quintess",
        "==============================",
        "",
        "Quintess is licensed under MIT OR Apache-2.0 (see LICENSE-MIT and LICENSE-APACHE).",
        "It includes the software listed below, each under its own license.",
        "",
        "PDFium (rendering and text): BSD-3-Clause and the licenses of its parts, below.",
        "Binary build from pdfium-binaries by Benoit Blanchon (MIT), below.",
        "OCR models (text-detection.onnx, text-recognition.onnx): by Robert Knight,",
        "  from https://huggingface.co/robertknight/ocrs, licensed CC BY-SA 4.0",
        "  (https://creativecommons.org/licenses/by-sa/4.0/); trained on the HierText",
        "  dataset (CC BY-SA 4.0). They are included unmodified.",
        "Spelling dictionaries: English (SCOWL, by Kevin Atkinson and others, BSD-style),",
        "  Spanish (GPL/LGPL/MPL), French (MPL-2.0), German (GPL-2.0 or 3.0), Portuguese",
        "  (Brazil, LGPL-3.0/MPL-2.0) and Italian (GPL-3.0), from wooorm/dictionaries;",
        "  their license texts are in licenses/dictionaries. They are separate data files,",
        "  used unmodified.",
        "Oxi (oxidocs, oxidocs-core, oxidocs-common), the built-in Word layout engine: MPL-2.0",
        "  (https://mozilla.org/MPL/2.0/); its source is at https://github.com/Ryujiyasu/oxi.",
        "Icons (built into the app, used where Segoe Fluent Icons is missing, as on Linux):",
        "  a subset of Material Icons (Outlined) by Google, Apache-2.0, from",
        "  https://github.com/google/material-design-icons; text in its section below.",
        "",
        f"Rust crates ({len(packages)}):",
        "",
    ]
    for package in packages:
        system = only_on.get(package["id"])
        note = f"  ({system} only)" if system else ""
        lines.append(f"  {package['name']} {package['version']}  -  {package.get('license') or 'see its license file'}{note}")
    lines.append("")
    printed = {}
    for package in packages:
        folder = Path(package["manifest_path"]).parent
        header = f"{package['name']} {package['version']} ({package.get('license') or 'license file'})"
        lines += ["", "-" * 78, header, "-" * 78]
        texts = license_texts(folder)
        if not texts:
            lines.append(f"No license file is included in the crate; its declared license is {package.get('license')}.")
        for name, text in texts:
            digest = hashlib.sha256(text.encode()).hexdigest()
            if digest in printed:
                lines.append(f"{name}: the same text as {printed[digest]} above.")
            else:
                printed[digest] = f"{package['name']} ({name})"
                lines += [f"{name}:", "", text, ""]
    icons = ROOT / "assets" / "fonts" / "quintess-icons.LICENSE.txt"
    lines += ["", "=" * 78, "Material Icons (the app's fallback icons)", "=" * 78, "", icons.read_text(encoding="utf-8").strip()]
    # The Windows and Linux builds of PDFium come from the same release of
    # pdfium-binaries and carry the same license files.
    pdfium = ROOT / "vendor" / "pdfium"
    lines += ["", "=" * 78, "PDFium and its components", "=" * 78]
    for path in [pdfium / "LICENSE"] + sorted((pdfium / "licenses").glob("*")):
        if path.is_file():
            title = "pdfium-binaries build scripts" if path.parent == pdfium else path.stem
            lines += ["", "-" * 78, title, "-" * 78, path.read_text(encoding="utf-8", errors="replace").strip()]
    # Code adapted into the developer tools (tools/robustness), which are not
    # part of the programs; credited here because the MIT license asks for its
    # notice to travel with substantial portions of the code.
    pdfcraft = ROOT / "licenses" / "pdfcraft" / "LICENSE-MIT"
    lines += [
        "", "=" * 78, "PdfCraft (adapted into the developer tools, not the programs)", "=" * 78, "",
        "tools/robustness (the corpus fetch, the child-process runner, the baseline",
        "comparison and the mutation fuzzer) is adapted, with changes, from PdfCraft,",
        "https://github.com/storytold/pdfcraft (commit 6d843a8e8b22e4442666eb9e12f506c40458af3d),",
        "licensed MIT OR Apache-2.0. Its MIT license:", "",
        pdfcraft.read_text(encoding="utf-8").strip(),
    ]
    OUTPUT.parent.mkdir(exist_ok=True)
    OUTPUT.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"wrote {OUTPUT} ({len(packages)} crates)")


if __name__ == "__main__":
    main()
