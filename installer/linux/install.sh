#!/bin/sh
# Installs Quintess from this unpacked folder.
#
#   ./install.sh                 for you only, in ~/.local (no administrator rights)
#   sudo ./install.sh --system   for everyone: /opt/quintess, with the commands
#                                in /usr/local/bin and menu entries in /usr/local/share
#
# The programs and their files (PDFium, dictionaries, recognition models,
# licenses) go in one folder; `quintess` and `quintess-app` are linked from
# a bin folder, and a menu entry and icons are added. Installing again
# replaces an earlier version. To remove it, run uninstall.sh, which is
# also copied into the program folder.

set -eu

here=$(cd "$(dirname "$0")" && pwd)
mode=user

usage() {
    sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
}

for argument in "$@"; do
    case "$argument" in
        --system) mode=system ;;
        --user) mode=user ;;
        -h|--help) usage; exit 0 ;;
        *) echo "install.sh: unknown option $argument (try --help)" >&2; exit 2 ;;
    esac
done

if [ "$mode" = system ]; then
    if [ "$(id -u)" -ne 0 ]; then
        echo "install.sh: --system needs administrator rights; run: sudo $0 --system" >&2
        exit 1
    fi
    program_folder=/opt/quintess
    bin_folder=/usr/local/bin
    share_folder=/usr/local/share
else
    if [ "$(id -u)" -eq 0 ]; then
        echo "install.sh: running as root installs into root's home folder; use --system to install for everyone" >&2
    fi
    program_folder="$HOME/.local/lib/quintess"
    bin_folder="$HOME/.local/bin"
    share_folder="${XDG_DATA_HOME:-$HOME/.local/share}"
fi

if [ ! -f "$here/program/quintess-app" ]; then
    echo "install.sh: run this from the unpacked Quintess folder (program/quintess-app is missing)" >&2
    exit 1
fi

# An earlier installation is removed first, so no stale files are left.
if [ -f "$program_folder/installed-files.txt" ]; then
    while IFS= read -r installed; do
        [ -n "$installed" ] && rm -f "$installed"
    done < "$program_folder/installed-files.txt"
fi
rm -rf "$program_folder"

mkdir -p "$program_folder" "$bin_folder" "$share_folder/applications"
cp -R "$here/program/." "$program_folder/"
cp "$here/uninstall.sh" "$program_folder/uninstall.sh"
chmod 755 "$program_folder/quintess" "$program_folder/quintess-app" "$program_folder/uninstall.sh"

manifest="$program_folder/installed-files.txt"
: > "$manifest"

for command in quintess quintess-app; do
    ln -sf "$program_folder/$command" "$bin_folder/$command"
    echo "$bin_folder/$command" >> "$manifest"
done

# The menu entry names the installed program by its full path, so it works
# even when the bin folder is not on the PATH.
desktop_file="$share_folder/applications/quintess.desktop"
sed -e "s|^Exec=quintess-app|Exec=\"$program_folder/quintess-app\"|" \
    -e "s|^TryExec=.*|TryExec=$program_folder/quintess-app|" \
    "$here/share/applications/quintess.desktop" > "$desktop_file"
echo "$desktop_file" >> "$manifest"

for icon in "$here"/share/icons/hicolor/*/apps/quintess.png; do
    size_folder=$(basename "$(dirname "$(dirname "$icon")")")
    mkdir -p "$share_folder/icons/hicolor/$size_folder/apps"
    cp "$icon" "$share_folder/icons/hicolor/$size_folder/apps/quintess.png"
    echo "$share_folder/icons/hicolor/$size_folder/apps/quintess.png" >> "$manifest"
done

# Let the desktop notice the new menu entry, its PDF association and icon.
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$share_folder/applications" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1 && [ -f "$share_folder/icons/hicolor/index.theme" ]; then
    gtk-update-icon-cache -q -t -f "$share_folder/icons/hicolor" >/dev/null 2>&1 || true
fi

echo "Quintess is installed in $program_folder."
echo "Commands: $bin_folder/quintess-app (the app) and $bin_folder/quintess (the command-line tool)."

case ":$PATH:" in
    *":$bin_folder:"*) ;;
    *) echo "Note: $bin_folder is not on your PATH; add it to run quintess by name (the menu entry works either way)." ;;
esac

# Shared libraries the programs need that this system lacks.
if command -v ldd >/dev/null 2>&1; then
    missing=$(ldd "$program_folder/quintess-app" "$program_folder/quintess" 2>/dev/null | awk '/not found/ { print $1 }' | sort -u)
    if [ -n "$missing" ]; then
        echo "Warning: these libraries are missing: $(echo $missing)"
        echo "  On Debian and Ubuntu: sudo apt install libspeechd2 libgl1 libegl1 libxkbcommon0"
        echo "  On Fedora: sudo dnf install speech-dispatcher-libs mesa-libGL mesa-libEGL libxkbcommon"
    fi
fi

echo "Optional: cups-client (printing), sane-utils (scanning), speech-dispatcher (read aloud),"
echo "LibreOffice (converting Office files), xdg-utils (opening links), zenity (file dialogs"
echo "on desktops without a portal) and fonts-liberation (text editing stand-in fonts)."
