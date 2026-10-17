#!/bin/sh
# Copyright 2026 Marissa Tanner, Heronetic
# SPDX-License-Identifier: Apache-2.0
# Installs OpenHeronBrowser for the current user (~/.local).
set -e
here="$(cd "$(dirname "$0")" && pwd)"
prefix="${PREFIX:-$HOME/.local}"
mkdir -p "$prefix/bin" "$prefix/share/applications" "$prefix/share/icons/hicolor/256x256/apps"
install -m 755 "$here/ohb" "$prefix/bin/ohb"
install -m 644 "$here/openheronbrowser.png" "$prefix/share/icons/hicolor/256x256/apps/openheronbrowser.png"
sed "s|^Exec=ohb|Exec=$prefix/bin/ohb|" "$here/openheronbrowser.desktop" > "$prefix/share/applications/openheronbrowser.desktop"
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$prefix/share/applications" >/dev/null 2>&1 || true
echo "Installed to $prefix/bin/ohb — launch \"OpenHeronBrowser\" from your app menu or run: ohb"
