#!/usr/bin/env bash
# Build a static, stripped waypipe (shm only, lz4 + zstd) for a Linux musl target.
#
# Usage: build-waypipe.sh TARGET OUTPUT
#   TARGET  x86_64-unknown-linux-musl | aarch64-unknown-linux-musl |
#           armv7-unknown-linux-musleabihf
#   OUTPUT  path of the binary to write
#
# Approach: cargo-zigbuild, with zig as the C compiler and linker for every
# target. lz4 and zstd are built from pinned release tarballs as static
# libraries for the target and found through pkg-config. waypipe's lz4/zstd
# wrapper crates run the `bindgen` CLI, which is installed from crates.io.
# One small patch is applied to the waypipe sources (see below); the script
# itself is the record of that modification.
#
# Environment:
#   WAYPIPE_BUILD_DIR       work directory (default: ./build next to this script)
#   WAYPIPE_RUST_TOOLCHAIN  rustup toolchain to install and use, e.g. 1.100.0
#                           (default: whatever `cargo` resolves to)
#
# Host requirements: bash, git, curl, sha256sum, make, a host C compiler (cc,
# for build scripts), pkg-config, python3 with venv, file, readelf (binutils),
# libclang (for bindgen), and a Rust toolchain with cargo (rustup is used to
# add the target when present).
# On GitHub Actions ubuntu-latest (rustup, gcc, make, git, python3 preinstalled):
#   sudo apt-get install -y libclang-dev pkg-config
# A bare Ubuntu 24.04 also needs:
#   apt-get install curl git make gcc libc6-dev python3-venv file binutils
# zig, cargo-zigbuild and bindgen are installed by this script into
# $WAYPIPE_BUILD_DIR/tools.

set -euo pipefail

WAYPIPE_REPO=https://gitlab.freedesktop.org/mstoeckl/waypipe.git
WAYPIPE_TAG=v0.11.3
WAYPIPE_COMMIT=3ea049b2d33cbf1000714c28b9668f8c244f70bd
WAYPIPE_FEATURES=lz4,zstd

LZ4_VERSION=1.10.0
LZ4_SHA256=537512904744b35e232912055ccf8ec66d768639ff3abe5788d90d792ec5f48b
ZSTD_VERSION=1.5.7
ZSTD_SHA256=eb33e51f49a15e023950cd7825ca74a4a2b43db8354825ac24fc1b7ee09e6fa3

ZIGLANG_VERSION=0.16.0
CARGO_ZIGBUILD_VERSION=0.24.1
BINDGEN_CLI_VERSION=0.73.2

die() { echo "build-waypipe: $*" >&2; exit 1; }
log() { echo "==> $*" >&2; }

[ $# -eq 2 ] || die "usage: $0 TARGET OUTPUT"
TARGET=$1
OUTPUT=$2

case "$TARGET" in
  x86_64-unknown-linux-musl)
    ZIG_TARGET=x86_64-linux-musl ZIG_CPU=baseline ;;
  aarch64-unknown-linux-musl)
    ZIG_TARGET=aarch64-linux-musl ZIG_CPU=baseline ;;
  armv7-unknown-linux-musleabihf)
    ZIG_TARGET=arm-linux-musleabihf ZIG_CPU=generic+v7a+vfp3d16-d32+thumb2-neon ;;
  *)
    die "unsupported target: $TARGET" ;;
esac

for cmd in git curl sha256sum make cc pkg-config python3 cargo file readelf; do
  command -v "$cmd" >/dev/null || die "missing required command: $cmd"
done

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BUILD_DIR=${WAYPIPE_BUILD_DIR:-$SCRIPT_DIR/build}
mkdir -p "$BUILD_DIR"
BUILD_DIR=$(cd "$BUILD_DIR" && pwd)
TOOLS=$BUILD_DIR/tools
DL=$BUILD_DIR/downloads
SRC=$BUILD_DIR/waypipe-$WAYPIPE_TAG
DEPS=$BUILD_DIR/deps/$TARGET-lz4-$LZ4_VERSION-zstd-$ZSTD_VERSION-zig-$ZIGLANG_VERSION
mkdir -p "$TOOLS/bin" "$DL" "$DEPS"

# Keep zig and cargo-zigbuild caches inside the build directory.
export ZIG_GLOBAL_CACHE_DIR=$BUILD_DIR/cache/zig
export ZIG_LOCAL_CACHE_DIR=$BUILD_DIR/cache/zig-local
export CARGO_ZIGBUILD_CACHE_DIR=$BUILD_DIR/cache/cargo-zigbuild

if [ -n "${WAYPIPE_RUST_TOOLCHAIN:-}" ]; then
  rustup toolchain install --profile minimal --no-self-update \
    --target "$TARGET" "$WAYPIPE_RUST_TOOLCHAIN"
  export RUSTUP_TOOLCHAIN=$WAYPIPE_RUST_TOOLCHAIN
fi
if command -v rustup >/dev/null; then
  rustup target add "$TARGET" >/dev/null
fi
log "using $(rustc --version)"

# Tools: zig and cargo-zigbuild from PyPI in a venv, bindgen from crates.io.
if [ ! -x "$TOOLS/venv/bin/cargo-zigbuild" ] \
  || [ "$("$TOOLS/venv/bin/cargo-zigbuild" --version)" != "cargo-zigbuild $CARGO_ZIGBUILD_VERSION" ] \
  || [ "$("$TOOLS/venv/bin/python-zig" version)" != "$ZIGLANG_VERSION" ]; then
  log "installing ziglang $ZIGLANG_VERSION and cargo-zigbuild $CARGO_ZIGBUILD_VERSION"
  python3 -m venv "$TOOLS/venv"
  "$TOOLS/venv/bin/pip" install --quiet --disable-pip-version-check \
    "ziglang==$ZIGLANG_VERSION" "cargo-zigbuild==$CARGO_ZIGBUILD_VERSION"
fi
if [ "$("$TOOLS/bin/bindgen" --version 2>/dev/null)" != "bindgen $BINDGEN_CLI_VERSION" ]; then
  log "installing bindgen-cli $BINDGEN_CLI_VERSION"
  cargo install --quiet --locked --force --root "$TOOLS" \
    bindgen-cli --version "$BINDGEN_CLI_VERSION"
fi
export PATH="$TOOLS/bin:$TOOLS/venv/bin:$PATH"

# cargo-zigbuild locates zig through the ziglang Python package.
ZIG="$TOOLS/venv/bin/python-zig"
cat > "$TOOLS/bin/zcc-$TARGET" <<EOF
#!/bin/sh
exec "$ZIG" cc -target $ZIG_TARGET -mcpu=$ZIG_CPU "\$@"
EOF
cat > "$TOOLS/bin/zar" <<EOF
#!/bin/sh
exec "$ZIG" ar "\$@"
EOF
chmod +x "$TOOLS/bin/zcc-$TARGET" "$TOOLS/bin/zar"

fetch() { # url sha256 dest
  if [ ! -f "$3" ] || ! echo "$2  $3" | sha256sum -c --status; then
    log "downloading $1"
    curl -fsSL --retry 3 -o "$3.part" "$1"
    echo "$2  $3.part" | sha256sum -c --status || die "checksum mismatch for $1"
    mv "$3.part" "$3"
  fi
}

# waypipe source, pinned by tag and verified by commit hash.
if [ ! -d "$SRC/.git" ]; then
  log "cloning waypipe $WAYPIPE_TAG"
  git -c advice.detachedHead=false clone --quiet --depth 1 --branch "$WAYPIPE_TAG" \
    "$WAYPIPE_REPO" "$SRC"
fi
[ "$(git -C "$SRC" rev-parse HEAD)" = "$WAYPIPE_COMMIT" ] \
  || die "waypipe $WAYPIPE_TAG is not commit $WAYPIPE_COMMIT"

# The wrapper build scripts print an extra `rustc-link-lib=<name>` (dylib kind)
# after pkg-config has already emitted the library. rustc turns that into
# `-Wl,-Bdynamic -l<name>`, which zig's linker refuses to satisfy from a .a.
# Dropping the duplicate leaves pkg-config's `static=` link line in place.
git -C "$SRC" checkout --quiet -- .
git -C "$SRC" apply - <<'EOF'
--- a/wrap-lz4/build.rs
+++ b/wrap-lz4/build.rs
@@ -40,8 +40,6 @@ fn main() {

     let lib = pkg_config::probe_library("liblz4").unwrap();

-    println!("cargo:rustc-link-lib=lz4");
-
     let mut includes = Vec::new();
     includes.extend_from_slice(&lib.include_paths[..]);

--- a/wrap-zstd/build.rs
+++ b/wrap-zstd/build.rs
@@ -40,8 +40,6 @@ fn main() {

     let lib = pkg_config::probe_library("libzstd").unwrap();

-    println!("cargo:rustc-link-lib=zstd");
-
     let mut includes = Vec::new();
     includes.extend_from_slice(&lib.include_paths[..]);

EOF

# Static lz4 and zstd for the target, installed into $DEPS with .pc files.
if [ ! -f "$DEPS/lib/liblz4.a" ]; then
  fetch "https://github.com/lz4/lz4/releases/download/v$LZ4_VERSION/lz4-$LZ4_VERSION.tar.gz" \
    "$LZ4_SHA256" "$DL/lz4-$LZ4_VERSION.tar.gz"
  log "building lz4 $LZ4_VERSION for $TARGET"
  rm -rf "$DEPS/src/lz4" && mkdir -p "$DEPS/src/lz4"
  tar -xzf "$DL/lz4-$LZ4_VERSION.tar.gz" -C "$DEPS/src/lz4" --strip-components=1
  CFLAGS="-O3 -fPIC" make -C "$DEPS/src/lz4/lib" -j"$(nproc)" install \
    CC="zcc-$TARGET" AR=zar BUILD_SHARED=no PREFIX="$DEPS" >/dev/null
fi
if [ ! -f "$DEPS/lib/libzstd.a" ]; then
  fetch "https://github.com/facebook/zstd/releases/download/v$ZSTD_VERSION/zstd-$ZSTD_VERSION.tar.gz" \
    "$ZSTD_SHA256" "$DL/zstd-$ZSTD_VERSION.tar.gz"
  log "building zstd $ZSTD_VERSION for $TARGET"
  rm -rf "$DEPS/src/zstd" && mkdir -p "$DEPS/src/zstd"
  tar -xzf "$DL/zstd-$ZSTD_VERSION.tar.gz" -C "$DEPS/src/zstd" --strip-components=1
  CFLAGS="-O3 -fPIC" make -C "$DEPS/src/zstd/lib" -j"$(nproc)" \
    install-static install-includes install-pc \
    CC="zcc-$TARGET" AR=zar ZSTD_LEGACY_SUPPORT=0 PREFIX="$DEPS" >/dev/null
fi

# Only the target's own .pc files are visible, and they are used statically.
export PKG_CONFIG_LIBDIR="$DEPS/lib/pkgconfig"
export PKG_CONFIG_PATH=
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_ALL_STATIC=1

# Release profile without debug info, fully stripped, with fat LTO.
export CARGO_PROFILE_RELEASE_DEBUG=false
export CARGO_PROFILE_RELEASE_STRIP=symbols
export CARGO_PROFILE_RELEASE_LTO=true
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
# Keep build machine paths out of panic messages. With the rust-src component
# installed, std paths point into the local sysroot instead of /rustc/<hash>.
CARGO_HOME_DIR=${CARGO_HOME:-$HOME/.cargo}
RUST_SRC=$(rustc --print sysroot)/lib/rustlib/src/rust
RUST_COMMIT=$(rustc -vV | sed -n 's/^commit-hash: //p')
CARGO_ENCODED_RUSTFLAGS=$(printf '%s\x1f%s\x1f%s' \
  "--remap-path-prefix=$SRC=/waypipe" \
  "--remap-path-prefix=$CARGO_HOME_DIR=/cargo" \
  "--remap-path-prefix=$RUST_SRC=/rustc/$RUST_COMMIT")
export CARGO_ENCODED_RUSTFLAGS
unset RUSTFLAGS

log "building waypipe $WAYPIPE_TAG for $TARGET (features: $WAYPIPE_FEATURES)"
# Run from the source tree: zig prints its include paths relative to the
# working directory when they are below it, and cargo-zigbuild hands those
# paths to bindgen, which runs in a different directory.
(cd "$SRC" && cargo zigbuild --locked --release \
  --target-dir "$BUILD_DIR/target" \
  --target "$TARGET" \
  --no-default-features --features "$WAYPIPE_FEATURES" \
  --bin waypipe)

BIN=$BUILD_DIR/target/$TARGET/release/waypipe
if readelf -lW "$BIN" | grep -q 'Requesting program interpreter'; then
  die "$BIN has a program interpreter (not static)"
fi
if readelf -dW "$BIN" | grep -q '(NEEDED)'; then
  die "$BIN needs shared libraries (not static)"
fi

mkdir -p "$(dirname "$OUTPUT")"
cp "$BIN" "$OUTPUT"
file "$OUTPUT"
