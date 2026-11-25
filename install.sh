#!/usr/bin/env bash
# waytrans Rust v2 installer script
set -euo pipefail

PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
OLD_LIB_DIR="$PREFIX/lib/waytrans"

echo "=== Installing waytrans (Rust v2) ==="

# Check basic runtime dependencies
missing=()
for cmd in grim slurp tesseract wl-copy; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        missing+=("$cmd")
    fi
done

if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo (Rust toolchain) is required to build waytrans."
    echo "  Arch/CachyOS: sudo pacman -S rust"
    exit 1
fi

if [ ${#missing[@]} -gt 0 ]; then
    echo "⚠️ Warning: The following runtime tools are not currently installed:"
    for m in "${missing[@]}"; do
        echo "   - $m"
    done
    echo ""
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "Building release binary..."
cargo build --release --manifest-path "$SCRIPT_DIR/Cargo.toml"

mkdir -p "$BIN_DIR"
rm -rf "$OLD_LIB_DIR"
cp "$SCRIPT_DIR/target/release/waytrans" "$BIN_DIR/waytrans"
chmod +x "$BIN_DIR/waytrans"

INSTALLED_VER="$("$BIN_DIR/waytrans" --version 2>/dev/null || echo "waytrans")"
echo "✓ Installed $INSTALLED_VER to $BIN_DIR/waytrans"

if [[ ":$PATH:" != *":$BIN_DIR:"* ]]; then
    echo "ℹ Note: Make sure $BIN_DIR is added to your PATH in your shell config."
fi

read -rp "Would you like to download high-accuracy Japanese OCR models now? [Y/n]: " setup_choice || setup_choice="y"
if [[ "$setup_choice" =~ ^[Yy]?$ ]]; then
    "$BIN_DIR/waytrans" --setup-models
fi

echo ""
echo "=== Installation Finished! ==="
echo "You can now bind 'waytrans' to a shortcut in your Wayland compositor:"
echo ""
echo "Hyprland (~/.config/hypr/custom/keybinds.lua):"
echo "  hl.bind(\"SUPER + ALT + J\", hl.dsp.exec_cmd(HOME .. \"/.local/bin/waytrans -s ja -t en\"))"
echo "  hl.bind(\"SUPER + ALT + L\", hl.dsp.exec_cmd(HOME .. \"/.local/bin/waytrans --live -s ja -t en\"))"
echo ""
