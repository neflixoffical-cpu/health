#!/usr/bin/env bash
# Health — release build for Linux.
# Result: ../dist/health (single binary).
#   ./build.sh            build
#   ./build.sh --install  build and install to ~/.local/bin (no root)
set -euo pipefail
src="$(cd "$(dirname "$0")" && pwd)"
root="$(dirname "$src")"
command -v cargo >/dev/null || { echo "Rust (cargo) not found: sudo pacman -S rust" >&2; exit 1; }
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target}"
cd "$src"
echo "==> cargo build --release -p health"
cargo build --release --locked -p health
mkdir -p "$root/dist"
install -m755 "$CARGO_TARGET_DIR/release/health" "$root/dist/health"
echo "==> $root/dist/health"
if [[ "${1:-}" == "--install" ]]; then
  install -Dm755 "$root/dist/health" "$HOME/.local/bin/health"
  echo "==> installed to ~/.local/bin/health"
  case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) echo "   add ~/.local/bin to PATH";; esac
fi
