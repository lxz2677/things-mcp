#!/bin/sh
set -eu

: "${release_url:?Download install.sh from a things-mcp GitHub Release}"

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
bin_dir="${HOME:?Set HOME to your home directory}/.local/bin"
destination="$bin_dir/things-mcp"
if [ -d "$destination" ]; then
  printf 'Choose a different location for the directory at %s.\n' "$destination" >&2
  exit 1
fi
mkdir -p "$bin_dir"
work_dir=$(mktemp -d "$bin_dir/.things-mcp.XXXXXX")
trap 'rm -rf "$work_dir"' EXIT

(
  cd "$work_dir"
  curl -fsSL --proto '=https' "$release_url/$archive" -o "$archive"
  curl -fsSL --proto '=https' "$release_url/SHA256SUMS" -o SHA256SUMS
  awk -v archive="$archive" '$2 == archive' SHA256SUMS > archive.sha256
  if [ ! -s archive.sha256 ]; then
    printf 'Missing SHA-256 checksum for %s.\n' "$archive" >&2
    exit 1
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c archive.sha256
  else
    shasum -a 256 -c archive.sha256
  fi
  tar -xzf "$archive" things-mcp
  ./things-mcp --version
)

mv -f "$work_dir/things-mcp" "$destination"
printf 'Installed %s\n' "$destination"
