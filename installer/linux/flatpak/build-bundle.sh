#!/usr/bin/env bash
# Repackages a Quintess Linux tarball as a single-file Flatpak bundle:
#
#   <output>/Quintess-<version>-x86_64.flatpak   (and its .sha256)
#
# Usage: installer/linux/flatpak/build-bundle.sh TARBALL [OUTPUT_FOLDER]
#
# TARBALL is quintess-<version>-linux-x86_64.tar.gz from tools/package_linux.py;
# OUTPUT_FOLDER defaults to the tarball's folder. Needs flatpak and
# flatpak-builder; the freedesktop runtime and SDK the manifest names are
# installed for this user from Flathub. The manifest is
# cc.quintess.Quintess.yml beside this script.
#
# Adapted from PdfCraft's packaging/linux/flatpak-bundle.sh
# (https://github.com/storytold/pdfcraft, commit 6d843a8e8b22e4442666eb9e12f506c40458af3d),
# Copyright (c) 2026 ArtCraft Team and the PdfCraft contributors, MIT OR Apache-2.0.
# Modified: Quintess's names and layout, a required tarball argument, a
# checksum file, and testing left to the release workflow.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
app_id=cc.quintess.Quintess
flathub=https://dl.flathub.org/repo/flathub.flatpakrepo

if [ $# -lt 1 ] || [ "$1" = -h ] || [ "$1" = --help ]; then
    sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
fi
tarball="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
output="$(mkdir -p "${2:-$(dirname "$tarball")}" && cd "${2:-$(dirname "$tarball")}" && pwd)"

# quintess-0.3.0-linux-x86_64.tar.gz -> 0.3.0
version="$(basename "$tarball" | sed -n 's/^quintess-\(.*\)-linux-x86_64\.tar\.gz$/\1/p')"
[ -n "$version" ] || { echo "build-bundle.sh: $tarball is not named quintess-<version>-linux-x86_64.tar.gz" >&2; exit 1; }
for tool in flatpak flatpak-builder; do
    command -v "$tool" >/dev/null || { echo "build-bundle.sh: $tool is missing (apt install flatpak flatpak-builder)" >&2; exit 1; }
done

echo "==> Quintess $version Flatpak bundle from $(basename "$tarball")"

# Everything flatpak-builder writes goes in one work folder, removed first.
work="$output/flatpak-work"
rm -rf "$work"
mkdir -p "$work/stage"
tar -xzf "$tarball" -C "$work/stage" --strip-components=1
[ -x "$work/stage/program/quintess" ] || { echo "build-bundle.sh: $tarball has no program/quintess" >&2; exit 1; }
cp "$here/$app_id.yml" "$work/$app_id.yml"

flatpak remote-add --user --if-not-exists flathub "$flathub"
# --disable-rofiles-fuse: there is no FUSE in containers and on CI runners.
flatpak-builder --user --install-deps-from=flathub --disable-rofiles-fuse --force-clean \
    --default-branch=stable --repo="$work/repo" --state-dir="$work/state" "$work/build" "$work/$app_id.yml"

bundle="$output/Quintess-$version-x86_64.flatpak"
# --runtime-repo lets `flatpak install` fetch the runtime from Flathub when it's missing.
flatpak build-bundle --runtime-repo="$flathub" "$work/repo" "$bundle" "$app_id" stable
(cd "$output" && sha256sum "$(basename "$bundle")" > "$(basename "$bundle").sha256")
echo "wrote $bundle ($(du -h "$bundle" | cut -f1))"
