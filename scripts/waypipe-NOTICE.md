# waypipe in rembro releases

The `waypipe-x86_64`, `waypipe-aarch64` and `waypipe-armv7` binaries in rembro's
releases are builds of [waypipe](https://gitlab.freedesktop.org/mstoeckl/waypipe)
0.11.3 (commit 3ea049b2d33cbf1000714c28b9668f8c244f70bd) by Manuel Stoeckl and
contributors, licensed under the GNU General Public License, version 3 or later
(`waypipe-GPL-3.0.txt`). rembro copies one into a target that has no waypipe of
its own and runs it there as a separate program.

They are built from modified source. On 2026-11-21 the rembro project removed one
`println!("cargo:rustc-link-lib=...")` line from each of `wrap-lz4/build.rs` and
`wrap-zstd/build.rs`, so that lz4 and zstd link statically. `build-waypipe.sh`
contains that change and the full build procedure. The builds have waypipe's
`lz4` and `zstd` features and none of its others (no dmabuf, GBM or video).

The binaries also contain:

- lz4 1.10.0, BSD 2-Clause license (`lz4-BSD-2-Clause.txt`)
- zstd 1.5.7, BSD 3-Clause license (`zstd-BSD-3-Clause.txt`)
- musl libc, MIT license
- Rust crates under the MIT and Apache-2.0 licenses, listed in waypipe's
  `Cargo.lock`

The complete corresponding source, including the vendored crates, the lz4 and
zstd release tarballs and the build script, is `waypipe-source.tar.gz` in the
same GitHub release as the binaries.
