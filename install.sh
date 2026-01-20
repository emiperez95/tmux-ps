#!/usr/bin/env bash
# Installation script for tmux-ps

set -e

INSTALL_DIR="$HOME/.local/bin"
SCRIPT_NAME="tmux-ps"

echo "Installing tmux-ps..."

# Create install directory if it doesn't exist
if [ ! -d "$INSTALL_DIR" ]; then
    echo "Creating $INSTALL_DIR..."
    mkdir -p "$INSTALL_DIR"
fi

# Copy the script
echo "Copying tmux-ps to $INSTALL_DIR..."
cp "$(dirname "$0")/$SCRIPT_NAME" "$INSTALL_DIR/$SCRIPT_NAME"
chmod +x "$INSTALL_DIR/$SCRIPT_NAME"

# Check if ~/.local/bin is in PATH
if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
    echo ""
    echo "⚠️  $INSTALL_DIR is not in your PATH."
    echo ""
    echo "Add this line to your ~/.zshrc or ~/.bashrc:"
    echo ""
    echo "    export PATH=\"\$HOME/.local/bin:\$PATH\""
    echo ""
    echo "Then run: source ~/.zshrc (or ~/.bashrc)"
else
    echo "✓ Installation complete!"
    echo ""
    echo "Usage:"
    echo "  tmux-ps           # Show all processes"
    echo "  tmux-ps --compact # Show only high-resource processes"
fi
