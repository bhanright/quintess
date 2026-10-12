"""
Builds the Linux release of Quintess and packages it:

    quintess-<version>-linux-x86_64.tar.gz   unpack, then ./install.sh (or --system)
    quintess_<version>_amd64.deb             sudo apt install ./quintess_<version>_amd64.deb
    Quintess-<version>-x86_64.AppImage       with --appimage: chmod +x, then run it

each with a matching .sha256 file ("<hex>  <name>", as sha256sum writes it).
The Flatpak bundle is made from the tarball by installer/linux/flatpak/build-bundle.sh.

    python3 tools/package_linux.py [--skip-build] [--pdfium DIR] [--models DIR] [--output DIR]
                                   [--appimage --appimagetool FILE --appimage-runtime FILE]

Run it on Linux (the release workflow uses Ubuntu 22.04, whose glibc 2.35
sets the oldest system the program runs on). It needs Rust, and for the
build the packages listed in docs/BUILDING.md. It does not need dpkg: both
archives are written here, with root ownership and exact permissions.

Layout, mirroring the Windows installer (installer/quintess.iss), where
everything sits in one folder beside the program:

    <program folder>/quintess, libpdfium.so
    <program folder>/dictionaries/   Hunspell dictionaries
    <program folder>/models/         OCR models
    <program folder>/licenses/       LICENSE-MIT, LICENSE-APACHE, notices, PDFium's licenses

The program folder is /usr/lib/quintess for the .deb (linked from
/usr/bin), ~/.local/lib/quintess or /opt/quintess for the tarball,
usr/lib/quintess inside the AppImage and /app/lib/quintess in the Flatpak.
The program finds these files through its own location, following the
links, so nothing is configured at install time.
"""

import argparse
import gzip
import hashlib
import io
import lzma
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tarfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
INSTALLER = ROOT / "installer" / "linux"
ICONS = ROOT / "assets" / "linux" / "icons" / "hicolor"
DICTIONARIES = ROOT / "assets" / "dictionaries"
# The reverse-DNS name the AppImage and the Flatpak use for the desktop file,
# the icon and the AppStream metadata (the site is quintess.cc). The .deb and
# the tarball keep the plain name "quintess".
APP_ID = "cc.quintess.Quintess"
METAINFO_TEMPLATE = INSTALLER / f"{APP_ID}.metainfo.xml.in"
# One program: the app, and with a command the command-line tool.
PROGRAMS = ["quintess"]

# The OCR models, checked against the checksums the program itself demands
# (crates/quintess-core/src/ocr.rs).
MODELS = {
    "text-detection.onnx": "a917b23dbd9524b465df7e922641b3ff2981623df4ded5a0234004ef2fee7cfe",
    "text-recognition.onnx": "86c145c2edb96c8caed5b1ebb8f44d706408922211451c309c625157dd6061c5",
}

# Libraries the app opens at run time rather than linking (so they don't
# show as needed in the program file): OpenGL and EGL for drawing, and
# xkbcommon, X11 and Wayland for the window, through winit.
RUNTIME_OPENED_PACKAGES = [
    "libgl1",
    "libegl1",
    "libxkbcommon0",
    # X11 sessions need the X11 half of xkbcommon too; without it the app stops at start.
    "libxkbcommon-x11-0",
    "libx11-6",
    "libx11-xcb1",
    "libxcursor1",
    "libxrandr2",
    "libxi6",
    "libwayland-client0",
    "libwayland-cursor0",
    "libwayland-egl1",
]

# Debian packages that provide each library the program links directly.
LINKED_LIBRARY_PACKAGES = {
    "libc.so.6": "libc6",
    "libm.so.6": "libc6",
    "libdl.so.2": "libc6",
    "libpthread.so.0": "libc6",
    "librt.so.1": "libc6",
    "ld-linux-x86-64.so.2": "libc6",
    "libgcc_s.so.1": "libgcc-s1",
    "libspeechd.so.2": "libspeechd2",
}

RECOMMENDS = [
    "cups-client",  # printing
    "speech-dispatcher",  # read aloud
    "xdg-utils",  # opening links, folders and reports
    "zenity | xdg-desktop-portal-gtk | xdg-desktop-portal-kde",  # file dialogs
    "fonts-liberation",  # stand-ins for Arial, Times New Roman and Courier New when editing text
    "fonts-noto-cjk",  # Chinese, Japanese and Korean text in stamps, comments and fields
    "fonts-dejavu-core",
]

SUGGESTS = [
    "sane-utils",  # scanning
    "libreoffice-writer | libreoffice",  # converting Office documents
    "chromium | chromium-browser | google-chrome-stable",  # converting web pages
    "fonts-noto-core",  # text in more scripts when editing
]

MAINTAINER = "William Hanright <bhanright@users.noreply.github.com>"
HOMEPAGE = "https://quintess.cc"
DESCRIPTION = """Private PDF reader and editor
 Quintess reads and edits PDF files on your own computer: edit text, sign,
 redact for real, fill and create forms, organize pages, OCR scans, compare,
 convert and automate. No account and no uploads; it works offline.
 .
 The quintess command opens the desktop app (quintess file.pdf) and, given
 a command, is the command-line tool (quintess --help)."""


def workspace_version():
    """The version in Cargo.toml's [workspace.package] (read without tomllib,
    which Ubuntu 22.04's Python 3.10 lacks)."""
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    section = text.split("[workspace.package]", 1)[1].split("\n[", 1)[0]
    found = re.search(r'^version\s*=\s*"([^"]+)"', section, re.MULTILINE)
    if not found:
        raise SystemExit("no version in Cargo.toml's [workspace.package]")
    return found.group(1)


def sha256_of(path):
    digest = hashlib.sha256()
    with open(path, "rb") as file:
        for block in iter(lambda: file.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


# --- Reading ELF files, to state dependencies honestly ----------------------


def elf_dynamic_info(path):
    """The libraries an ELF program links (DT_NEEDED) and the newest glibc
    symbol version it asks for, read without readelf or objdump."""
    data = pathlib.Path(path).read_bytes()
    if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
        raise SystemExit(f"{path} is not a 64-bit little-endian ELF file")

    def number(offset, size):
        return int.from_bytes(data[offset:offset + size], "little")

    section_offset, section_size, section_count = number(0x28, 8), number(0x3A, 2), number(0x3C, 2)
    sections = []
    for index in range(section_count):
        base = section_offset + index * section_size
        sections.append({"type": number(base + 4, 4), "offset": number(base + 0x18, 8), "size": number(base + 0x20, 8), "link": number(base + 0x28, 4)})
    needed = []
    for section in sections:
        if section["type"] != 6:  # SHT_DYNAMIC
            continue
        strings = sections[section["link"]]
        for entry in range(section["offset"], section["offset"] + section["size"], 16):
            tag, value = number(entry, 8), number(entry + 8, 8)
            if tag == 0:
                break
            if tag == 1:  # DT_NEEDED
                start = strings["offset"] + value
                needed.append(data[start:data.index(b"\0", start)].decode())
    versions = {tuple(int(part) for part in found.decode().split(".")) for found in re.findall(rb"GLIBC_(\d+\.\d+(?:\.\d+)?)", data)}
    newest = max(versions) if versions else None
    return needed, newest


def debian_depends(binaries):
    packages, newest_glibc = [], (2, 17)
    for binary in binaries:
        needed, newest = elf_dynamic_info(binary)
        if newest and newest > newest_glibc:
            newest_glibc = newest
        for library in needed:
            package = LINKED_LIBRARY_PACKAGES.get(library)
            if package is None:
                raise SystemExit(f"{binary.name} links {library}, which has no Debian package listed in package_linux.py; add it")
            if package not in packages:
                packages.append(package)
    glibc = ".".join(map(str, newest_glibc))
    depends = [f"libc6 (>= {glibc})" if package == "libc6" else package for package in packages]
    depends += [package for package in RUNTIME_OPENED_PACKAGES if package not in packages]
    return depends, glibc


# --- Building and staging --------------------------------------------------


def build(target, cargo_command):
    command = ["cargo", cargo_command, "--release", "-j", "6", "-p", "quintess-cli"]
    if target:
        command += ["--target", target]
    print("+", " ".join(command), flush=True)
    subprocess.run(command, cwd=ROOT, check=True)


def release_folder(target):
    base = pathlib.Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    if target:
        # cargo-zigbuild accepts a glibc version after the triple; the folder doesn't carry it.
        base = base / target.split(".")[0]
    return base / "release"


class Tree:
    """Files to package: archive path -> (source path or bytes, mode), plus links."""

    def __init__(self):
        self.files = {}
        self.links = {}

    def add(self, path, source, mode=0o644):
        self.files[path] = (source, mode)

    def add_folder(self, path, folder, pattern="*", mode=0o644):
        for source in sorted(pathlib.Path(folder).glob(pattern)):
            if source.is_file():
                self.add(f"{path}/{source.name}", source, mode)

    def link(self, path, target):
        self.links[path] = target

    def folders(self):
        found = set()
        for path in list(self.files) + list(self.links):
            parts = path.split("/")[:-1]
            for end in range(1, len(parts) + 1):
                found.add("/".join(parts[:end]))
        return sorted(found)

    def write_tar(self, archive, prefix="", mtime=None, owner_root=True):
        """Writes the tree into an open tarfile, folders first."""
        mtime = mtime or int(time.time())

        def info(name, kind, mode, size=0):
            entry = tarfile.TarInfo(name)
            entry.type, entry.mode, entry.size, entry.mtime = kind, mode, size, mtime
            if owner_root:
                entry.uid = entry.gid = 0
                entry.uname = entry.gname = "root"
            return entry

        for folder in self.folders():
            archive.addfile(info(prefix + folder + "/", tarfile.DIRTYPE, 0o755))
        for path in sorted(self.files):
            source, mode = self.files[path]
            data = source if isinstance(source, bytes) else pathlib.Path(source).read_bytes()
            archive.addfile(info(prefix + path, tarfile.REGTYPE, mode, len(data)), io.BytesIO(data))
        for path in sorted(self.links):
            entry = info(prefix + path, tarfile.SYMTYPE, 0o777)
            entry.linkname = self.links[path]
            archive.addfile(entry)

    def write_folder(self, folder):
        """Writes the tree into a real folder (an AppImage's AppDir)."""
        folder = pathlib.Path(folder)
        for path in sorted(self.files):
            source, mode = self.files[path]
            destination = folder / path
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(source if isinstance(source, bytes) else pathlib.Path(source).read_bytes())
            destination.chmod(mode)
        for path in sorted(self.links):
            destination = folder / path
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.symlink_to(self.links[path])


def program_folder_files(tree, prefix, binaries, pdfium, models, licenses_extra=()):
    """The program folder's contents under `prefix`."""
    for binary in binaries:
        tree.add(f"{prefix}/{binary.name}", binary, 0o755)
    tree.add(f"{prefix}/libpdfium.so", pdfium / "lib" / "libpdfium.so", 0o644)
    for name in MODELS:
        tree.add(f"{prefix}/models/{name}", models / name)
    tree.add_folder(f"{prefix}/dictionaries", DICTIONARIES, "*.aff")
    tree.add_folder(f"{prefix}/dictionaries", DICTIONARIES, "*.dic")
    tree.add_folder(f"{prefix}/licenses/dictionaries", DICTIONARIES, "*.license.txt")
    tree.add(f"{prefix}/licenses/LICENSE-MIT", ROOT / "LICENSE-MIT")
    tree.add(f"{prefix}/licenses/LICENSE-APACHE", ROOT / "LICENSE-APACHE")
    tree.add(f"{prefix}/licenses/THIRD-PARTY-NOTICES.txt", ROOT / "licenses" / "THIRD-PARTY-NOTICES.txt")
    tree.add_folder(f"{prefix}/licenses/pdfium", pdfium / "licenses")
    tree.add(f"{prefix}/licenses/pdfium/pdfium-binaries-build-scripts.txt", pdfium / "LICENSE")
    for name, source in licenses_extra:
        tree.add(f"{prefix}/licenses/{name}", source)


def desktop_integration_files(tree, share):
    tree.add(f"{share}/applications/quintess.desktop", INSTALLER / "quintess.desktop")
    for icon in sorted(ICONS.glob("*/apps/quintess.png")):
        size = icon.parent.parent.name
        tree.add(f"{share}/icons/hicolor/{size}/apps/quintess.png", icon)


def metainfo_text(version):
    """The AppStream metadata with this release's version and today's date."""
    template = METAINFO_TEMPLATE.read_text(encoding="utf-8")
    return template.replace("@VERSION@", version).replace("@DATE@", time.strftime("%Y-%m-%d", time.gmtime()))


def reverse_dns_desktop_file():
    """quintess.desktop with the icon named by APP_ID, for the AppImage,
    whose desktop file and icon are named that way. The window's class stays
    "quintess" (StartupWMClass), which is what the app sets."""
    text = (INSTALLER / "quintess.desktop").read_text(encoding="utf-8")
    if "\nIcon=quintess\n" not in text:
        raise SystemExit("installer/linux/quintess.desktop has no Icon=quintess line to rename")
    return text.replace("\nIcon=quintess\n", f"\nIcon={APP_ID}\n")


def write_checksum(path):
    checksum = path.with_name(path.name + ".sha256")
    checksum.write_text(f"{sha256_of(path)}  {path.name}\n", encoding="utf-8", newline="\n")
    return checksum


def make_tarball(output, version, glibc, binaries, pdfium, models):
    name = f"quintess-{version}-linux-x86_64"
    tree = Tree()
    program_folder_files(tree, "program", binaries, pdfium, models)
    desktop_integration_files(tree, "share")
    # Not installed by install.sh; the Flatpak, made from this tarball, uses it.
    tree.add(f"share/metainfo/{APP_ID}.metainfo.xml", metainfo_text(version).encode())
    tree.add("install.sh", INSTALLER / "install.sh", 0o755)
    tree.add("uninstall.sh", INSTALLER / "uninstall.sh", 0o755)
    tree.add("README.txt", README_TEXT.format(version=version, glibc=glibc).encode())
    path = output / f"{name}.tar.gz"
    with gzip.GzipFile(path, "wb", mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT) as archive:
            # Unpacked by the person, so files belong to whoever unpacks them.
            tree.write_tar(archive, prefix=f"{name}/", owner_root=False)
    return path


README_TEXT = """Quintess {version} for Linux (x86-64)

Install for yourself (into ~/.local, no administrator rights):
    ./install.sh
Install for everyone (into /opt/quintess):
    sudo ./install.sh --system
Remove:
    ~/.local/lib/quintess/uninstall.sh   (or: sudo /opt/quintess/uninstall.sh)

Then start Quintess from your desktop's menu, or run quintess (quintess file.pdf
opens a file). Followed by a command, quintess is the command-line tool
(quintess --help lists them).

Needs glibc {glibc} or later (see ldd --version) and libspeechd2. Optional: cups-client (printing), sane-utils (scanning),
speech-dispatcher (read aloud), LibreOffice (converting Office files).

The user guide: https://quintess.cc/guide/ (also F1 in the app).
Licenses: program/licenses.
"""


# --- The AppImage ------------------------------------------------------------


def system_library(name):
    """Where the dynamic linker finds a 64-bit library on this (build) system."""
    listing = subprocess.run(["ldconfig", "-p"], capture_output=True, text=True, check=True).stdout
    for line in listing.splitlines():
        found = re.match(rf"\s*{re.escape(name)} \(([^)]*)\) => (\S+)", line)
        if found and "x86-64" in found.group(1):
            return pathlib.Path(found.group(2)).resolve()
    raise SystemExit(f"{name} is not installed on this system (on Ubuntu: apt install libspeechd2)")


def make_appimage(output, version, binaries, pdfium, models, appimagetool, runtime):
    """An AppImage: the tarball's program folder under usr/lib/quintess, run
    by AppRun, plus libspeechd, which the tarball and the .deb take from the
    system but which not every distribution installs. The rest of what the
    program links or opens (glibc, OpenGL, X11, Wayland, xkbcommon) is left to
    the system, as AppImages do: those must match the system's drivers."""
    appdir = output / "Quintess.AppDir"
    if appdir.exists():
        shutil.rmtree(appdir)
    tree = Tree()
    speech_library = system_library("libspeechd.so.2")
    # libspeechd is LGPL-2.1+; its Debian copyright file carries the notice.
    speech_copyright = pathlib.Path("/usr/share/doc/libspeechd2/copyright")
    extra_licenses = [("libspeechd/copyright", speech_copyright)] if speech_copyright.is_file() else []
    program_folder_files(tree, "usr/lib/quintess", binaries, pdfium, models, extra_licenses)
    tree.add("usr/lib/quintess/libspeechd.so.2", speech_library)
    tree.link("usr/bin/quintess", "../lib/quintess/quintess")
    tree.add("AppRun", INSTALLER / "appimage" / "AppRun", 0o755)

    desktop_file = reverse_dns_desktop_file().encode()
    tree.add(f"{APP_ID}.desktop", desktop_file)
    tree.add(f"usr/share/applications/{APP_ID}.desktop", desktop_file)
    for icon in sorted(ICONS.glob("*/apps/quintess.png")):
        tree.add(f"usr/share/icons/hicolor/{icon.parent.parent.name}/apps/{APP_ID}.png", icon)
    tree.add(f"{APP_ID}.png", ICONS / "256x256" / "apps" / "quintess.png")
    tree.link(".DirIcon", f"{APP_ID}.png")
    tree.add(f"usr/share/metainfo/{APP_ID}.metainfo.xml", metainfo_text(version).encode())
    tree.write_folder(appdir)

    # The program finds libspeechd.so.2 beside itself. Only the AppImage's
    # copy of the program is changed; the .deb and tarball keep the original.
    subprocess.run(["patchelf", "--set-rpath", "$ORIGIN", str(appdir / "usr/lib/quintess/quintess")], check=True)

    path = output / f"Quintess-{version}-x86_64.AppImage"
    if path.exists():
        path.unlink()
    # appimagetool is itself an AppImage; CI runners may lack FUSE, so it
    # unpacks itself instead of mounting. --no-appstream: the workflow
    # validates the metadata separately with the distribution's appstreamcli.
    environment = dict(os.environ, ARCH="x86_64", APPIMAGE_EXTRACT_AND_RUN="1")
    command = [str(appimagetool), "--no-appstream", "--runtime-file", str(runtime), str(appdir), str(path)]
    print("+", " ".join(command), flush=True)
    subprocess.run(command, check=True, env=environment)
    return path


def ar_archive(members):
    """A Unix ar archive, as .deb files are: (name, bytes) in order."""
    out = io.BytesIO()
    out.write(b"!<arch>\n")
    now = int(time.time())
    for name, data in members:
        header = f"{name:<16}{now:<12}{0:<6}{0:<6}{0o100644:<8o}{len(data):<10}`\n".encode()
        assert len(header) == 60
        out.write(header)
        out.write(data)
        if len(data) % 2:
            out.write(b"\n")
    return out.getvalue()


def make_deb(output, version, binaries, pdfium, models):
    tree = Tree()
    program_folder_files(tree, "usr/lib/quintess", binaries, pdfium, models)
    # Upgrading from a version before 0.3.0, dpkg removes that package's
    # quintess-app program and its /usr/bin link, which this one lacks.
    for binary in binaries:
        tree.link(f"usr/bin/{binary.name}", f"../lib/quintess/{binary.name}")
    desktop_integration_files(tree, "usr/share")
    tree.add("usr/share/doc/quintess/copyright", debian_copyright().encode())
    tree.add("usr/share/doc/quintess/changelog.Debian.gz", gzip.compress(debian_changelog(version).encode(), mtime=0))

    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode="w", format=tarfile.GNU_FORMAT) as archive:
        tree.write_tar(archive, prefix="./")
    data_xz = lzma.compress(data.getvalue(), preset=6)

    depends, glibc = debian_depends(binaries)
    installed_kilobytes = sum(len(s) if isinstance(s, bytes) else pathlib.Path(s).stat().st_size for s, _ in tree.files.values()) // 1024 + 1
    control = "\n".join([
        "Package: quintess",
        f"Version: {version}",
        "Architecture: amd64",
        f"Maintainer: {MAINTAINER}",
        f"Installed-Size: {installed_kilobytes}",
        f"Depends: {', '.join(depends)}",
        f"Recommends: {', '.join(RECOMMENDS)}",
        f"Suggests: {', '.join(SUGGESTS)}",
        "Section: text",
        "Priority: optional",
        f"Homepage: {HOMEPAGE}",
        f"Description: {DESCRIPTION}",
    ]) + "\n"
    md5sums = "".join(f"{hashlib.md5(source if isinstance(source, bytes) else pathlib.Path(source).read_bytes()).hexdigest()}  {path}\n" for path, (source, _) in sorted(tree.files.items()))
    control_tree = Tree()
    control_tree.add("control", control.encode())
    control_tree.add("md5sums", md5sums.encode())
    control_data = io.BytesIO()
    with tarfile.open(fileobj=control_data, mode="w", format=tarfile.GNU_FORMAT) as archive:
        archive.addfile(_root_folder("./"))
        control_tree.write_tar(archive, prefix="./")
    control_gz = gzip.compress(control_data.getvalue(), mtime=0)

    path = output / f"quintess_{version}_amd64.deb"
    path.write_bytes(ar_archive([("debian-binary", b"2.0\n"), ("control.tar.gz", control_gz), ("data.tar.xz", data_xz)]))
    return path, control, glibc


def _root_folder(name):
    entry = tarfile.TarInfo(name)
    entry.type, entry.mode, entry.mtime = tarfile.DIRTYPE, 0o755, int(time.time())
    entry.uname = entry.gname = "root"
    return entry


def debian_copyright():
    return """Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: Quintess
Source: https://github.com/bhanright/quintess

Files: *
Copyright: 2026 William Hanright
License: MIT or Apache-2.0
 The full texts are in /usr/lib/quintess/licenses (LICENSE-MIT and
 LICENSE-APACHE). Quintess includes PDFium (BSD-3-Clause and its parts'
 licenses, in /usr/lib/quintess/licenses/pdfium), the ocrs recognition
 models (CC BY-SA 4.0), spelling dictionaries under their own licenses
 (/usr/lib/quintess/licenses/dictionaries), Material Icons (Apache-2.0) and
 about 400 Rust libraries, all listed with their license texts in
 /usr/lib/quintess/licenses/THIRD-PARTY-NOTICES.txt.
"""


def debian_changelog(version):
    date = time.strftime("%a, %d %b %Y %H:%M:%S +0000", time.gmtime())
    return f"quintess ({version}) stable; urgency=medium\n\n  * Release {version}; see https://github.com/bhanright/quintess/releases\n\n -- {MAINTAINER}  {date}\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--skip-build", action="store_true", help="package the release binaries already built")
    parser.add_argument("--target", default="", help="a Cargo target triple, for cross builds")
    parser.add_argument("--cargo-command", default="build", help="build, or zigbuild for cargo-zigbuild cross builds")
    parser.add_argument("--pdfium", default=str(ROOT / "vendor" / "pdfium-linux"), help="unpacked pdfium-linux-x64.tgz (chromium/8086)")
    parser.add_argument("--models", default=str(ROOT / "vendor" / "ocrs-models"), help="folder with the two OCR models")
    parser.add_argument("--output", default=str(ROOT / "installer" / "output"), help="where the packages are written")
    parser.add_argument("--appimage", action="store_true", help="also write the AppImage (needs patchelf, --appimagetool and --appimage-runtime)")
    parser.add_argument("--appimagetool", help="appimagetool-x86_64.AppImage (AppImage/appimagetool 1.9.1)")
    parser.add_argument("--appimage-runtime", help="runtime-x86_64 from AppImage/type2-runtime, embedded in the AppImage")
    arguments = parser.parse_args()
    if arguments.appimage and not (arguments.appimagetool and arguments.appimage_runtime):
        parser.error("--appimage needs --appimagetool and --appimage-runtime")

    version = workspace_version()
    pdfium, models, output = pathlib.Path(arguments.pdfium), pathlib.Path(arguments.models), pathlib.Path(arguments.output)
    if not (pdfium / "lib" / "libpdfium.so").is_file():
        raise SystemExit(f"libpdfium.so not found in {pdfium}/lib; unpack pdfium-linux-x64.tgz from pdfium-binaries chromium/8086 there")
    for name, expected in MODELS.items():
        if not (models / name).is_file():
            raise SystemExit(f"{models / name} is missing; `quintess ocr` downloads the models once, or see docs/BUILDING.md")
        if sha256_of(models / name) != expected:
            raise SystemExit(f"{models / name} is not the expected model (its checksum differs)")
    if not (ROOT / "licenses" / "THIRD-PARTY-NOTICES.txt").is_file():
        raise SystemExit("licenses/THIRD-PARTY-NOTICES.txt is missing; run tools/third_party_notices.py")

    if not arguments.skip_build:
        build(arguments.target, arguments.cargo_command)
    binaries = [release_folder(arguments.target) / name for name in PROGRAMS]
    for binary in binaries:
        if not binary.is_file():
            raise SystemExit(f"{binary} was not built")

    output.mkdir(parents=True, exist_ok=True)
    _, glibc = debian_depends(binaries)
    tarball = make_tarball(output, version, glibc, binaries, pdfium, models)
    deb, control, glibc = make_deb(output, version, binaries, pdfium, models)
    packages = [tarball, deb]
    if arguments.appimage:
        packages.append(make_appimage(output, version, binaries, pdfium, models, pathlib.Path(arguments.appimagetool).resolve(), pathlib.Path(arguments.appimage_runtime).resolve()))
    for package in packages:
        write_checksum(package)
        print(f"wrote {package} ({package.stat().st_size / 1e6:.1f} MB)")
    print(f"needs glibc {glibc} or later")
    print(control)


if __name__ == "__main__":
    main()
