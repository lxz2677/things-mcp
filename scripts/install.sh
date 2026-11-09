#!/bin/sh
set -eu

latest_release_url=https://github.com/lxz2677/things-mcp/releases/latest/download
release_url=$latest_release_url
mode=install
if [ "${1-}" = --update ]; then
  mode=update
  release_url=$latest_release_url
  shift
fi
if [ "$#" -ne 0 ]; then
  printf 'Usage: sh install.sh [--update]\n' >&2
  exit 1
fi

cd "${HOME:?Set HOME to your home directory}" || exit 1

case "$(uname -s)" in
  Darwin) platform=macos ;;
  Linux) platform=linux ;;
  *) echo "things-mcp supports macOS and Linux." >&2; exit 1 ;;
esac

case "$(uname -m)" in
  arm64|aarch64) arch=arm64 ;;
  x86_64) arch=x86_64 ;;
  *) echo "things-mcp supports ARM64 and x86_64." >&2; exit 1 ;;
esac

archive="things-mcp-$platform-$arch.tar.gz"
install_dir=${THINGS_MCP_INSTALL_DIR:-}
if [ -z "$install_dir" ] && [ -t 0 ]; then
  if [ "$mode" = update ]; then
    printf 'Existing install directory [%s]: ' "$HOME" >&2
  else
    printf 'Install directory [%s]: ' "$HOME" >&2
  fi
  IFS= read -r install_dir || exit 1
fi
install_dir=${install_dir:-$HOME}
case "$install_dir" in
  '~') install_dir="$HOME" ;;
  '~/'*) install_dir="$HOME/${install_dir#'~/'}" ;;
  /*) ;;
  *) install_dir="$HOME/$install_dir" ;;
esac
if [ "$mode" = update ]; then
  if [ ! -f "$install_dir/things-mcp" ] || [ ! -x "$install_dir/things-mcp" ]; then
    printf 'No executable things-mcp found in %s. Choose the original install directory.\n' "$install_dir" >&2
    exit 1
  fi
else
  mkdir -p "$install_dir" || exit 1
fi
cd -P "$install_dir" || exit 1

destination="$PWD/things-mcp"
if [ -d "$destination" ]; then
  printf '%s is a directory. Move it before installing.\n' "$destination" >&2
  exit 1
fi
work_dir=$(mktemp -d "$PWD/.things-mcp-install.XXXXXX") || exit 1
trap 'rm -rf "$work_dir"' EXIT

cd "$work_dir" || exit 1
curl -fsSL --proto '=https' "$release_url/$archive" -o "$archive" &&
  curl -fsSL --proto '=https' "$release_url/SHA256SUMS" -o SHA256SUMS &&
  awk -v archive="$archive" '$2 == archive' SHA256SUMS > archive.sha256 || exit 1
if [ ! -s archive.sha256 ]; then
  printf 'Missing SHA-256 checksum for %s.\n' "$archive" >&2
  exit 1
fi
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum -c archive.sha256
else
  shasum -a 256 -c archive.sha256
fi || exit 1

tar -xzf "$archive" things-mcp &&
  ./things-mcp --version || exit 1
if [ "$mode" = update ] && cmp -s things-mcp "$destination"; then
  printf 'Already up to date at %s\n' "$destination"
  exit 0
fi
mv -f things-mcp "$destination" || exit 1
if [ "$mode" = update ]; then
  printf 'Updated %s\n' "$destination"
else
  printf 'Installed %s\n' "$destination"
fi
