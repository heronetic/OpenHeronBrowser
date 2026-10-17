#!/bin/bash
# Copyright 2026 Marissa Tanner, Heronetic
# SPDX-License-Identifier: Apache-2.0
# Cross-compiles OpenHeronBrowser release binaries from Linux x86_64 for
# Linux (x86_64, arm64), Windows x64 and macOS (arm64, x86_64).
#
# Requirements:
#   rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu \
#       x86_64-pc-windows-gnu aarch64-apple-darwin x86_64-apple-darwin
#   cargo install cargo-zigbuild && pip install ziglang     (Linux glibc 2.17 builds, macOS)
#   mingw-w64                                                (Windows: apt install mingw-w64)
#   SDKROOT=/path/to/MacOSX14.5.sdk                          (a macOS SDK, for the macOS builds)
# Each build is a fat-LTO release build; on a 2-core machine expect 10-25 min each.
set -e
cd "$(dirname "$0")/.."
LOG=${LOG:-/tmp/ohb-build.log}
: > "$LOG"
step() { echo "== $1" | tee -a "$LOG"; }

step "linux x86_64 (glibc 2.17)"
cargo zigbuild --release -j 2 --target x86_64-unknown-linux-gnu.2.17 >> "$LOG" 2>&1

step "linux arm64 (glibc 2.17)"
cargo zigbuild --release -j 2 --target aarch64-unknown-linux-gnu.2.17 >> "$LOG" 2>&1

step "windows x86_64"
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
  cargo build --release -j 2 --target x86_64-pc-windows-gnu >> "$LOG" 2>&1

step "macos arm64"
SDKROOT=${SDKROOT:?set SDKROOT to a macOS SDK} MACOSX_DEPLOYMENT_TARGET=11.0 \
  cargo zigbuild --release -j 2 --target aarch64-apple-darwin >> "$LOG" 2>&1

step "macos x86_64"
SDKROOT=$SDKROOT MACOSX_DEPLOYMENT_TARGET=10.13 \
  cargo zigbuild --release -j 2 --target x86_64-apple-darwin >> "$LOG" 2>&1

step "done"
