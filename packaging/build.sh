#!/bin/sh
# Builds the npm package and the PyPI wheel from the release builds in DIST:
#   packaging/build.sh VERSION DIST OUT
#
# OUT gets npm/rembro-VERSION.tgz and pypi/rembro-VERSION-py3-none-any.whl. Each holds the six
# rembro builds and a launcher that runs the one for its system.

set -eu

[ $# -eq 3 ] || {
	echo "usage: $0 VERSION DIST OUT" >&2
	exit 2
}
version=$1
dist=$2
out=$3
here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")
builds="rembro-x86_64 rembro-aarch64 rembro-armv7 rembro-macos-aarch64 rembro-macos-x86_64 rembro-windows-x86_64.exe"

mkdir -p "$out/npm" "$out/pypi"
out=$(cd "$out" && pwd)
files=$(mktemp -d)
trap 'rm -rf "$files"' EXIT

for b in $builds; do
	cp "$dist/$b" "$files/"
	chmod 755 "$files/$b"
done
cp "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$files/"
# npm and PyPI show the README without the repository around it, so its links go to the
# repository at this version.
sed -e "s#](docs/#](https://github.com/ScriptSmith/rembro/blob/v$version/docs/#g" \
	-e "s#src=\"docs/#src=\"https://raw.githubusercontent.com/ScriptSmith/rembro/v$version/docs/#g" \
	"$root/README.md" >"$files/README.md"

mkdir "$files/npm"
cp "$here/npm/package.json" "$here/npm/rembro.js" "$files/README.md" "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$files/npm/"
for b in $builds; do
	cp -p "$files/$b" "$files/npm/"
done
chmod 755 "$files/npm/rembro.js"
(
	cd "$files/npm"
	npm pkg set version="$version"
	npm pack --pack-destination "$out/npm"
)

python3 "$here/pypi/build-wheel.py" "$version" "$files" "$out/pypi"
