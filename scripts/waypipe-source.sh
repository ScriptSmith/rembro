#!/usr/bin/env bash
# Collect what the GPL asks to accompany the waypipe binaries built by
# build-waypipe.sh: the corresponding source and the license texts.
#
# Usage: waypipe-source.sh BUILD_DIR OUTDIR
#   BUILD_DIR  the WAYPIPE_BUILD_DIR that build-waypipe.sh used
#   OUTDIR     where to write waypipe-source.tar.gz and licenses/
set -euo pipefail

[ $# -eq 2 ] || { echo "usage: $0 BUILD_DIR OUTDIR" >&2; exit 1; }
BUILD_DIR=$(cd "$1" && pwd)
mkdir -p "$2"
OUT=$(cd "$2" && pwd)
SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

SRC=$(ls -d "$BUILD_DIR"/waypipe-v*)
VERSION=${SRC##*/waypipe-v}
NAME=waypipe-$VERSION-rembro
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
mkdir "$STAGE/$NAME"

# Upstream source as tagged; build-waypipe.sh holds the patch applied to it.
git -C "$SRC" archive --format=tar --prefix=waypipe-$VERSION/ HEAD | tar -x -C "$STAGE/$NAME"
(cd "$SRC" && cargo vendor --locked --quiet "$STAGE/$NAME/vendor" >/dev/null)
cp "$BUILD_DIR"/downloads/lz4-*.tar.gz "$BUILD_DIR"/downloads/zstd-*.tar.gz "$STAGE/$NAME/"
cp "$SCRIPT_DIR/build-waypipe.sh" "$SCRIPT_DIR/waypipe-NOTICE.md" "$STAGE/$NAME/"
tar -czf "$OUT/waypipe-source.tar.gz" -C "$STAGE" "$NAME"

mkdir -p "$OUT/licenses"
cp "$SRC/LICENSE.GPLv3" "$OUT/licenses/waypipe-GPL-3.0.txt"
DEPS=$(ls -d "$BUILD_DIR"/deps/* | head -n 1)
cp "$DEPS/src/lz4/lib/LICENSE" "$OUT/licenses/lz4-BSD-2-Clause.txt"
cp "$DEPS/src/zstd/LICENSE" "$OUT/licenses/zstd-BSD-3-Clause.txt"
cp "$SCRIPT_DIR/waypipe-NOTICE.md" "$OUT/licenses/"
