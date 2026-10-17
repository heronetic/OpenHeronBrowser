# Building OpenHeronBrowser

You need Rust 1.90 or newer (install with [rustup](https://rustup.rs)) and a C compiler (for the bundled mimalloc allocator and the `ring` crypto library). The release profile uses fat LTO, so a build takes 10–25 minutes; the binary is `target/release/ohb` (`ohb.exe` on Windows).

For quick iteration, turn LTO off:

```
CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 cargo build --release
```

## macOS (Apple Silicon or Intel)

```
xcode-select --install          # the C compiler (Command Line Tools)
cargo build --release
```

To make an app bundle you can put in Applications:

```
mkdir -p OpenHeronBrowser.app/Contents/MacOS OpenHeronBrowser.app/Contents/Resources
cp target/release/ohb OpenHeronBrowser.app/Contents/MacOS/OpenHeronBrowser
cp packaging/macos/Info.plist OpenHeronBrowser.app/Contents/
cp assets/ohb.icns OpenHeronBrowser.app/Contents/Resources/
codesign --force --deep -s - OpenHeronBrowser.app      # ad-hoc signature
```

For a universal app (both architectures), build each and join them:

```
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin
lipo -create -output OpenHeronBrowser.app/Contents/MacOS/OpenHeronBrowser \
  target/aarch64-apple-darwin/release/ohb target/x86_64-apple-darwin/release/ohb
```

## Linux (x86_64 or arm64)

On the machine you're building for:

```
sudo apt install build-essential        # Debian/Ubuntu; Fedora: sudo dnf install gcc
cargo build --release
./target/release/ohb
```

The binary links only against the C library. The windowing and keyboard libraries (X11 or Wayland, `libxkbcommon`) are loaded at run time and come with any desktop session. To install it into your app menu, put `target/release/ohb` next to the files in `packaging/linux/`, write the icon with `./ohb --write-icon openheronbrowser.png 256`, and run `install.sh`.

To cross-compile an arm64 build from an x86_64 machine (or the reverse), use [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild); the `.2.17` suffix targets glibc 2.17, so the binary runs on old distributions too:

```
cargo install cargo-zigbuild && pip install ziglang
rustup target add aarch64-unknown-linux-gnu
cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.17   # → target/aarch64-unknown-linux-gnu/release/ohb
```

## Windows x64

On Windows, with the GNU toolchain. It embeds the icon and version information through `windres`.

```
rustup toolchain install stable-x86_64-pc-windows-gnu
rustup default stable-x86_64-pc-windows-gnu
cargo build --release                    # → target\release\ohb.exe
```

The GNU toolchain needs MinGW-w64 (`gcc` and `windres`) on the `PATH`, for example from [MSYS2](https://www.msys2.org) (`pacman -S mingw-w64-x86_64-gcc`). The default MSVC toolchain (with the Visual Studio C++ Build Tools) has not been tested, and its build has no embedded icon.

Cross-compiling from Linux:

```
sudo apt install mingw-w64
rustup target add x86_64-pc-windows-gnu
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
  cargo build --release --target x86_64-pc-windows-gnu      # → target/x86_64-pc-windows-gnu/release/ohb.exe
```

## Every platform at once

From Linux x86_64, with the tools above and a macOS SDK (`SDKROOT`), `packaging/build-all.sh` builds all five binaries. `packaging/package.sh` then makes the packages listed under [Downloads](README.md#downloads) in `dist/`, plus a source archive.

The `vendor/` directory carries patched copies of Boa (`boa_engine`, `boa_gc`, `boa_parser`, `boa_ast`, `boa_string`) and of the `url` crate. Changes are marked `OHB patch` (and debugging hooks `OHB debug aid`) in the source.

## Debugging switches

| Variable / flag | What it does |
|---|---|
| `OHB_CONSOLE=1` | Print `console.*` to stderr |
| `OHB_PROFILE=1` | Script parse/run times, style and layout totals |
| `OHB_STYLE_STATS=1`, `OHB_LAYOUT_STATS=1` | Each style pass / layout with timings |
| `OHB_GC_STATS=1`, `OHB_ALLOC_STATS=1` | Garbage-collector phases; allocations by type and by JS function |
| `OHB_DEBUG_NET=1` | Log requests |
| `OHB_INJECT=file.js` | Run a script before any page script |
| `--screenshot out.png URL --eval 'js'` | Headless render, then evaluate script and print the result |
| `--input "click:x,y;type:hi;key:Enter;wait:500"` | Drive the page with real input events |

## Testing

- **web-platform-tests:** `tools/wpt/wptrun.py --real <wpt checkout> <dirs…>` runs WPT directories against a local `./wpt serve`.
- **Pixel and layout comparisons against Chromium:** `tools/compare/` records sites into a web archive that both engines replay byte-for-byte, then compares pixels and box geometry.
