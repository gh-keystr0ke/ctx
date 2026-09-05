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
#   CTX_INSTALL_PYRIGHT  set to "0" to skip the optional Pyright Type Server
set -eu

repo="gh-keystr0ke/ctx"
install_dir="${CTX_INSTALL_DIR:-$HOME/.local/bin}"
pyright_version="1.1.413"
pyright_source_sha256="e417ff1a3d6eb838b68ae4219860762af1a9c2cdd7b71976c8e7ab1480e974e2"

say() { printf '%s\n' "$*" >&2; }
die() {
  say "error: $*"
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "'$1' is required but not found on PATH"
}

install_pyright_launcher() {
  runtime_dir_name=".ctx-pyright-typeserver-$pyright_version"
  launcher="$work_dir/pyright-typeserver"
  cat >"$launcher" <<EOF
#!/bin/sh
set -eu
script_dir=\$(CDPATH= cd -P "\$(dirname "\$0")" && pwd)
exec node "\$script_dir/$runtime_dir_name/pyright-typeserver.js" "\$@"
EOF
  cp "$launcher" "$install_dir/pyright-typeserver" || return 1
  chmod +x "$install_dir/pyright-typeserver" || return 1
}

install_pyright_typeserver() {
  case "${CTX_INSTALL_PYRIGHT:-1}" in
    0|false|no)
      say "skipping optional Pyright Type Server (CTX_INSTALL_PYRIGHT=0)"
      return
      ;;
    1|true|yes) ;;
    *) die "CTX_INSTALL_PYRIGHT must be 0 or 1" ;;
  esac

  if command -v pyright-typeserver >/dev/null 2>&1; then
    say "Pyright Type Server is already available on PATH"
    return
  fi
  if ! command -v node >/dev/null 2>&1 || ! command -v npm >/dev/null 2>&1; then
    say "note: Pyright Type Server was not installed; Node.js 18.12+ with npm is required for 'ctx infer-types'"
    return
  fi
  if ! node -e 'const [major, minor] = process.versions.node.split(".").map(Number); process.exit(major > 18 || (major === 18 && minor >= 12) ? 0 : 1)'; then
    say "note: Pyright Type Server was not installed; Node.js 18.12+ is required for its pinned build"
    return
  fi

  runtime_dir="$install_dir/.ctx-pyright-typeserver-$pyright_version"
  if [ -f "$runtime_dir/pyright-typeserver.js" ] && [ -d "$runtime_dir/dist" ]; then
    if ! install_pyright_launcher; then
      say "note: Pyright Type Server launcher could not be installed; use --pyright $runtime_dir/pyright-typeserver.js"
      return
    fi
    say "installed Pyright Type Server launcher to $install_dir/pyright-typeserver"
    return
  fi

  source_archive="$work_dir/pyright-$pyright_version.tar.gz"
  source_url="https://codeload.github.com/microsoft/pyright/tar.gz/refs/tags/$pyright_version"
  say "downloading pinned Pyright Type Server source ($pyright_version)..."
  if ! curl -fsSL -o "$source_archive" "$source_url"; then
    say "note: Pyright Type Server download failed; ctx is installed, but 'ctx infer-types' requires --pyright <path>"
    return
  fi
  if ! printf '%s  %s\n' "$pyright_source_sha256" "$source_archive" | shasum -a 256 -c - >/dev/null; then
    say "note: Pyright Type Server source checksum failed; refusing to build unverified source"
    return
  fi
  if ! tar xzf "$source_archive" -C "$work_dir"; then
    say "note: Pyright Type Server source could not be extracted; ctx remains installed"
    return
  fi
  source_dir="$work_dir/pyright-$pyright_version"
  npm_user_config="${NPM_CONFIG_USERCONFIG:-$HOME/.npmrc}"
  npm_build_config="$work_dir/npmrc"
  if [ -f "$npm_user_config" ]; then
    if ! sed '/^[[:space:]]*allow-scripts[[:space:]]*=/d' "$npm_user_config" >"$npm_build_config"; then
      say "note: npm configuration could not be prepared; ctx remains installed without Pyright Type Server"
      return
    fi
  elif ! : >"$npm_build_config"; then
    say "note: npm configuration could not be prepared; ctx remains installed without Pyright Type Server"
    return
  fi
  build_helpers="$work_dir/pyright-build-helpers"
  mkdir -p "$build_helpers"

  say "building Pyright Type Server (one-time npm dependency download)..."
  if ! (
    NPM_CONFIG_USERCONFIG="$npm_build_config" npm install \
      --prefix "$build_helpers" --ignore-scripts --no-save --no-audit --no-fund \
      glob@11.1.0 jsonc-parser@3.3.1
    NPM_CONFIG_USERCONFIG="$npm_build_config" npm ci \
      --prefix "$source_dir/packages/pyright-internal" \
      --ignore-scripts --no-audit --no-fund
    NPM_CONFIG_USERCONFIG="$npm_build_config" npm ci \
      --prefix "$source_dir/packages/pyright-typeserver" \
      --ignore-scripts --no-audit --no-fund
    NODE_PATH="$build_helpers/node_modules" \
      NPM_CONFIG_USERCONFIG="$npm_build_config" \
      npm --prefix "$source_dir/packages/pyright-typeserver" run build
  ); then
    say "note: Pyright Type Server build failed; ctx is installed, but 'ctx infer-types' requires --pyright <path>"
    return
  fi

  package_dir="$source_dir/packages/pyright-typeserver"
  if ! mkdir -p "$runtime_dir" \
    || ! cp "$package_dir/pyright-typeserver.js" "$runtime_dir/pyright-typeserver.js" \
    || ! cp -R "$package_dir/dist" "$runtime_dir/dist" \
    || ! install_pyright_launcher; then
    say "note: Pyright Type Server build succeeded but could not be installed; ctx remains installed"
    return
  fi
  say "installed Pyright Type Server $pyright_version to $install_dir/pyright-typeserver"
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

install_pyright_typeserver

case ":$PATH:" in
  *":$install_dir:"*) ;;
  *) say "note: $install_dir is not on your PATH — add it, e.g. export PATH=\"$install_dir:\$PATH\"" ;;
esac

"$install_dir/ctx" --version >&2 || true
