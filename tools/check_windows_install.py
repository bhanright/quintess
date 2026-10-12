"""
Checks that an installed copy of Quintess on Windows has exactly the files
the installer is meant to put there, so the installers built by
.github/workflows/windows-release.yml match a local build of
installer/quintess.iss file for file.

    python tools/check_windows_install.py <installed folder> <pdfium folder>

The pdfium folder is the unpacked pdfium-binaries archive the installer was
built from (vendor/pdfium, or vendor/pdfium-win-arm64), whose license files
are installed. The expected list mirrors the [Files] section of
installer/quintess.iss; change both together. Exits with 1 and lists the
differences when they don't match.
"""

import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
DICTIONARIES = ROOT / "assets" / "dictionaries"


def expected_files(pdfium):
    files = {
        "quintess.exe",
        "quintess.com",
        "pdfium.dll",
        "models/text-detection.onnx",
        "models/text-recognition.onnx",
        "licenses/LICENSE-MIT",
        "licenses/LICENSE-APACHE",
        "licenses/THIRD-PARTY-NOTICES.txt",
        "licenses/pdfium/pdfium-binaries-build-scripts.txt",
        # Inno Setup's uninstaller and its record of what was installed.
        "unins000.exe",
        "unins000.dat",
    }
    for pattern in ("*.aff", "*.dic"):
        files.update(f"dictionaries/{path.name}" for path in DICTIONARIES.glob(pattern))
    files.update(f"licenses/dictionaries/{path.name}" for path in DICTIONARIES.glob("*.license.txt"))
    files.update(f"licenses/pdfium/{path.name}" for path in (pdfium / "licenses").iterdir() if path.is_file())
    return files


def installed_files(folder):
    return {path.relative_to(folder).as_posix() for path in folder.rglob("*") if path.is_file()}


def main():
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    folder, pdfium = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
    expected, found = expected_files(pdfium), installed_files(folder)
    missing, unexpected = sorted(expected - found), sorted(found - expected)
    for name in missing:
        print(f"missing:    {name}")
    for name in unexpected:
        print(f"unexpected: {name}")
    if missing or unexpected:
        raise SystemExit(f"{folder} does not have the expected layout")
    print(f"{folder}: all {len(expected)} files as expected")


if __name__ == "__main__":
    main()
