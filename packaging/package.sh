#!/bin/bash
# Copyright 2026 Marissa Tanner, Heronetic
# SPDX-License-Identifier: Apache-2.0
# Packages the cross-compiled binaries into distributable archives in dist/.
set -e
cd "$(dirname "$0")/.."
V=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
OUT=dist
rm -rf "$OUT" && mkdir -p "$OUT"
T=target
LIPO=${LIPO:-$(command -v llvm-lipo || command -v llvm-lipo-18 || command -v lipo)}
CODESIGN=${CODESIGN:-$(command -v rcodesign || true)}

# ---------------- Linux (x86_64 and arm64)
for arch in x86_64 aarch64; do
  name=$([ $arch = aarch64 ] && echo arm64 || echo x86_64)
  L=$OUT/OpenHeronBrowser-$V-linux-$name
  mkdir -p "$L"
  cp $T/$arch-unknown-linux-gnu/release/ohb "$L/ohb"
  cp packaging/linux/install.sh packaging/linux/openheronbrowser.desktop "$L/"
  # The icon is drawn by OHB itself (the x86_64 build runs on the packaging host).
  "$T/x86_64-unknown-linux-gnu/release/ohb" --write-icon "$L/openheronbrowser.png" 256
  cp README.md LICENSE NOTICE THIRD_PARTY_NOTICES.md "$L/"
  chmod 755 "$L/ohb" "$L/install.sh"
  tar -C "$OUT" -czf "$OUT/OpenHeronBrowser-$V-linux-$name.tar.gz" "$(basename "$L")"
done

# ---------------- Windows
W=$OUT/OpenHeronBrowser-$V-windows-x64
mkdir -p "$W"
cp $T/x86_64-pc-windows-gnu/release/ohb.exe "$W/OpenHeronBrowser.exe"
cp README.md LICENSE NOTICE THIRD_PARTY_NOTICES.md "$W/"
(cd "$OUT" && zip -qr "OpenHeronBrowser-$V-windows-x64.zip" "$(basename "$W")")

# ---------------- macOS (universal .app)
M=$OUT/mac
APP=$M/OpenHeronBrowser.app
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
"$LIPO" -create -output "$APP/Contents/MacOS/OpenHeronBrowser" \
  $T/aarch64-apple-darwin/release/ohb $T/x86_64-apple-darwin/release/ohb
chmod 755 "$APP/Contents/MacOS/OpenHeronBrowser"
cp packaging/macos/Info.plist "$APP/Contents/Info.plist"
cp assets/ohb.icns "$APP/Contents/Resources/ohb.icns"
printf 'APPL????' > "$APP/Contents/PkgInfo"
if [ -n "$CODESIGN" ]; then
  "$CODESIGN" sign "$APP" >/dev/null
fi
cp README.md LICENSE NOTICE THIRD_PARTY_NOTICES.md "$M/"
(cd "$M" && zip -qry "../OpenHeronBrowser-$V-macos-universal.zip" OpenHeronBrowser.app README.md LICENSE NOTICE THIRD_PARTY_NOTICES.md)

# ---------------- Source
git archive --format=tar.gz --prefix=OpenHeronBrowser-$V-source/ -o "$OUT/OpenHeronBrowser-$V-source.tar.gz" HEAD

ls -la "$OUT"/*.zip "$OUT"/*.tar.gz
