#!/bin/sh
# Removes Quintess installed with install.sh.
#
#   ./uninstall.sh                 the installation in ~/.local
#   sudo ./uninstall.sh --system   the installation in /opt/quintess
#
# install.sh copies this script into the program folder, so it can also be
# run from there (~/.local/lib/quintess/uninstall.sh or
# /opt/quintess/uninstall.sh). Settings, the personal dictionary and
# downloaded models in ~/.config/quintess and ~/.local/share/quintess are
# kept; delete those folders too to remove everything.

set -eu

here=$(cd "$(dirname "$0")" && pwd)
mode=""

for argument in "$@"; do
    case "$argument" in
        --system) mode=system ;;
        --user) mode=user ;;
        -h|--help) sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "uninstall.sh: unknown option $argument (try --help)" >&2; exit 2 ;;
    esac
done

# Run from an installed program folder, that is the installation to remove.
if [ -z "$mode" ] && [ -f "$here/installed-files.txt" ]; then
    program_folder="$here"
elif [ "$mode" = system ]; then
    program_folder=/opt/quintess
else
    program_folder="$HOME/.local/lib/quintess"
fi

if [ ! -f "$program_folder/installed-files.txt" ]; then
    echo "uninstall.sh: no installation found in $program_folder" >&2
    exit 1
fi
if [ ! -w "$program_folder" ]; then
    echo "uninstall.sh: $program_folder needs administrator rights to remove; run: sudo $0 --system" >&2
    exit 1
fi

share_folders=""
while IFS= read -r installed; do
    [ -n "$installed" ] || continue
    rm -f "$installed"
    case "$installed" in
        */applications/quintess.desktop) share_folders="$share_folders $(dirname "$(dirname "$installed")")" ;;
    esac
done < "$program_folder/installed-files.txt"

rm -rf "$program_folder"

for share_folder in $share_folders; do
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$share_folder/applications" >/dev/null 2>&1 || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1 && [ -f "$share_folder/icons/hicolor/index.theme" ]; then
        gtk-update-icon-cache -q -t -f "$share_folder/icons/hicolor" >/dev/null 2>&1 || true
    fi
done

echo "Quintess is removed from $program_folder. Your settings in ~/.config/quintess were kept."
