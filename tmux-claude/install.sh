#!/usr/bin/env bash
# Installation script for tmux-claude

set -e

INSTALL_DIR="$HOME/.local/bin"
BINARY_NAME="tmux-claude"

echo "Installing tmux-claude..."

# Check for Rust/cargo
if ! command -v cargo &> /dev/null; then
    echo "Error: cargo not found. Please install Rust from https://rustup.rs/"
    exit 1
fi

# Build the release binary
echo "Building release binary..."
cargo build --release

# Create install directory if it doesn't exist
if [ ! -d "$INSTALL_DIR" ]; then
    echo "Creating $INSTALL_DIR..."
    mkdir -p "$INSTALL_DIR"
fi

# Copy the binary
echo "Copying tmux-claude to $INSTALL_DIR..."
cp "$(dirname "$0")/target/release/$BINARY_NAME" "$INSTALL_DIR/$BINARY_NAME"
chmod +x "$INSTALL_DIR/$BINARY_NAME"

# Check if ~/.local/bin is in PATH
if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
    echo ""
    echo "Warning: $INSTALL_DIR is not in your PATH."
    echo ""
    echo "Add this line to your ~/.zshrc or ~/.bashrc:"
    echo ""
    echo "    export PATH=\"\$HOME/.local/bin:\$PATH\""
    echo ""
    echo "Then run: source ~/.zshrc (or ~/.bashrc)"
else
    echo "Installation complete!"
    echo ""
    echo "Usage:"
    echo "  tmux-claude              # Interactive dashboard (2s refresh)"
    echo "  tmux-claude -w 5         # Custom refresh interval"
    echo "  tmux-claude -f pattern   # Filter sessions by name"
    echo "  tmux-claude --help       # Show all options"
fi
