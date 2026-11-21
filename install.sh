#!/bin/sh
# Installs rembro from its GitHub releases:
#   curl -fsSL https://raw.githubusercontent.com/ScriptSmith/rembro/main/install.sh | sh
#
# REMBRO_VERSION picks a release (default: the latest), and REMBRO_PREFIX picks where
# it goes (default: ~/.local, so rembro lands in ~/.local/lib/rembro and ~/.local/bin).

set -eu

main() {
	repo=ScriptSmith/rembro
	prefix=${REMBRO_PREFIX:-$HOME/.local}
	lib=$prefix/lib/rembro
	bin=$prefix/bin

	[ "$(uname -s)" = Linux ] || die "rembro runs on Linux only"
	case $(uname -m) in
	x86_64 | amd64) arch=x86_64 ;;
	aarch64 | arm64) arch=aarch64 ;;
	armv7l | armv7 | armv8l) arch=armv7 ;;
	*) die "there is no rembro build for $(uname -m)" ;;
	esac

	case ${REMBRO_VERSION:-latest} in
	latest) base=https://github.com/$repo/releases/latest/download ;;
	v*) base=https://github.com/$repo/releases/download/$REMBRO_VERSION ;;
	*) base=https://github.com/$repo/releases/download/v$REMBRO_VERSION ;;
	esac

	if command -v curl >/dev/null 2>&1; then
		fetch() { curl -fsSL --retry 2 -o "$2" "$1"; }
	elif command -v wget >/dev/null 2>&1; then
		fetch() { wget -qO "$2" "$1"; }
	else
		die "curl or wget is needed to download rembro"
	fi
	command -v sha256sum >/dev/null 2>&1 || die "sha256sum is needed to check the download"

	mkdir -p "$prefix/lib" "$bin"
	tmp=$(mktemp -d "$prefix/lib/.rembro-install.XXXXXX")
	trap 'rm -rf "$tmp"' EXIT
	trap 'exit 1' INT TERM

	pkg=rembro-$arch-linux
	echo "Downloading $pkg.tar.gz from $base"
	fetch "$base/$pkg.tar.gz" "$tmp/$pkg.tar.gz" || die "could not download $base/$pkg.tar.gz"
	fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "could not download $base/SHA256SUMS"

	want=$(awk -v f="$pkg.tar.gz" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")
	got=$(sha256sum "$tmp/$pkg.tar.gz" | cut -d' ' -f1)
	[ -n "$want" ] || die "$pkg.tar.gz is not in the release's SHA256SUMS"
	[ "$want" = "$got" ] || die "$pkg.tar.gz does not match its checksum (expected $want, got $got)"

	tar -xzf "$tmp/$pkg.tar.gz" -C "$tmp"
	[ -x "$tmp/$pkg/rembro" ] || die "$pkg.tar.gz has no rembro in it"

	# Swap the whole directory with renames, so a running rembro keeps its files.
	rm -rf "$lib.old"
	if [ -e "$lib" ]; then mv "$lib" "$lib.old"; fi
	mv "$tmp/$pkg" "$lib"
	rm -rf "$lib.old"
	ln -sf "$lib/rembro" "$bin/rembro"

	echo "Installed $("$lib/rembro" --version) to $bin/rembro"
	case :$PATH: in
	*:"$bin":*) ;;
	*) echo "$bin is not in your PATH. Add it to your shell's startup file, for example:
  export PATH=\"$bin:\$PATH\"" ;;
	esac
}

die() {
	echo "rembro install: $*" >&2
	exit 1
}

main "$@"
