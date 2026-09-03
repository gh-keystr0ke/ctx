#!/bin/sh
# Downloads and installs the latest (or a pinned) ctx release for the
# current OS/architecture from GitHub Releases, verifying its checksum.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/gh-keystr0ke/ctx/main/install.sh | sh
#
# Env overrides:
#   CTX_INSTALL_VERSION  pin a version instead of the latest (e.g. "0.6.1")
#   CTX_INSTALL_DIR      install directory (default: "$HOME/.local/bin")
set -eu

repo="gh-keystr0ke/ctx"
install_dir="${CTX_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*" >&2; }
die() {
  say "error: $*"
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "'$1' is required but not found on PATH"
}

need curl
need tar
need shasum

os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Darwin)
    case "$arch" in
      arm64) target="aarch64-apple-darwin" ;;
      x86_64) target="x86_64-apple-darwin" ;;
      *) die "unsupported macOS architecture: $arch" ;;
    esac
    ;;
  Linux)
    case "$arch" in
      x86_64) target="x86_64-unknown-linux-musl" ;;
      aarch64|arm64) target="aarch64-unknown-linux-musl" ;;
      *) die "unsupported Linux architecture: $arch (x86_64 and ARM64 builds are published)" ;;
    esac
    ;;
  *)
    die "unsupported OS: $os (only macOS and Linux builds are published)"
    ;;
esac

if [ -n "${CTX_INSTALL_VERSION:-}" ]; then
  tag="v${CTX_INSTALL_VERSION#v}"
else
  say "resolving latest release..."
  latest_json="$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest")" \
    || die "could not reach the GitHub releases API"
  tag="$(printf '%s' "$latest_json" | grep -m1 '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"
  [ -n "$tag" ] || die "could not resolve the latest release tag (GitHub API rate-limited? try CTX_INSTALL_VERSION=x.y.z)"
fi
version="${tag#v}"

archive="ctx-$version-$target.tar.gz"
base_url="https://github.com/$repo/releases/download/$tag"

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT INT TERM

say "downloading $archive ($tag)..."
curl -fsSL -o "$work_dir/$archive" "$base_url/$archive" \
  || die "download failed: $base_url/$archive"
curl -fsSL -o "$work_dir/$archive.sha256" "$base_url/$archive.sha256" \
  || die "download failed: $base_url/$archive.sha256"

say "verifying checksum..."
( cd "$work_dir" && shasum -a 256 -c "$archive.sha256" >/dev/null ) \
  || die "checksum verification failed for $archive"

say "extracting..."
tar xzf "$work_dir/$archive" -C "$work_dir"
extracted_dir="$work_dir/ctx-$version-$target"

mkdir -p "$install_dir"
cp "$extracted_dir/ctx" "$install_dir/ctx"
chmod +x "$install_dir/ctx"

say "installed ctx $version to $install_dir/ctx"

case ":$PATH:" in
  *":$install_dir:"*) ;;
  *) say "note: $install_dir is not on your PATH — add it, e.g. export PATH=\"$install_dir:\$PATH\"" ;;
esac

"$install_dir/ctx" --version >&2 || true
