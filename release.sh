#!/usr/bin/env bash
# release.sh: the files a GitHub release carries (issue #52), so a package can track omabox by a
# release asset and its checksum instead of GitHub's generated archives.
#
#   ./release.sh            dist/omabox-X.Y.Z.tar.gz from the tag vX.Y.Z (X.Y.Z is VERSION) and
#                           dist/SHA256SUMS
#   ./release.sh --upload   the same, then attach both to that tag's GitHub release
#
# The tarball is `git archive` of the tag, so it holds exactly what the tag does, minus what
# .gitattributes marks export-ignore (demo media, spike/, .github/, the test oracle). git gzips with
# no timestamp: the same tag gives the same bytes, and the checksum can be checked against a rebuild.
# A tarball with crates/ in it is built first as a package builds it (#324 wave 6-flip, proposal D3):
# fresh, its crates fetched (--locked) into a CARGO_HOME of its own, then built with no network
# (--frozen); one that cannot is never uploaded.
set -euo pipefail

ROOT=$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)
die() { echo "release.sh: $*" >&2; exit 1; }

upload=0
case ${1:-} in --upload) upload=1 ;; "") ;; *) die "usage: ./release.sh [--upload]" ;; esac

v=$(cat "$ROOT/VERSION")
[[ $v =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "VERSION is not X.Y.Z: $v"
tag=v$v
# An annotated tag, as every release has: a lightweight one is a local mistake more often than not.
[ "$(git -C "$ROOT" cat-file -t "$tag" 2>/dev/null)" = tag ] || die "no annotated tag $tag (git tag -a $tag -m 'omabox $v')"

name=omabox-$v.tar.gz
out=$ROOT/dist
mkdir -p "$out"
git -C "$ROOT" archive --format=tar.gz --prefix="omabox-$v/" -o "$out/$name" "$tag"

# offline_build TARBALL: the package's sequence on a fresh extract; its binary answers --version.
offline_build() {
  local t rc=0
  t=$(mktemp -d) || return 1
  # (each step checked: set -e does nothing in a subshell whose status `||` reads)
  (
    tar -xzf "$1" -C "$t" || exit 1
    src=$t/omabox-$v
    [ -f "$src/Cargo.toml" ] || exit 0
    export CARGO_HOME=$t/cargo-home CARGO_TARGET_DIR=$t/target
    cargo fetch --locked --manifest-path "$src/Cargo.toml" || exit 1
    unshare -rn cargo build --frozen --release -q --manifest-path "$src/Cargo.toml" || exit 1
    [ "$("$t/target/release/omabox" --version 2>/dev/null | head -n 1)" = "omabox $v" ] ||
      { echo "release.sh: the binary built from the tarball does not say omabox $v" >&2; exit 1; }
    echo "built offline from the tarball: omabox $v"
  ) || rc=$?
  rm -rf "$t"
  return $rc
}
offline_build "$out/$name" || { rm -f "$out/$name"; die "the tarball does not build offline (as a package builds it): not released"; }
(cd "$out" && sha256sum "$name" > SHA256SUMS)
echo "$out/$name ($(du -h "$out/$name" | cut -f1))"
cat "$out/SHA256SUMS"

[ $upload = 1 ] || exit 0
gh release view "$tag" >/dev/null 2>&1 || die "no GitHub release $tag yet (gh release create $tag ...)"
gh release upload "$tag" "$out/$name" "$out/SHA256SUMS" --clobber
echo "attached to release $tag"
