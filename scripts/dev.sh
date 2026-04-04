#!/bin/bash

set -euo pipefail

# Parse arguments
BUILD_TYPE="debug"
if [[ "$#" -gt 0 && "$1" == "--release" ]]; then
    BUILD_TYPE="release"
fi

# Verify ~/.git-ai/bin and ~/.git-ai/config.json exist
if [[ ! -d "$HOME/.git-ai/bin" ]] || [[ ! -f "$HOME/.git-ai/config.json" ]]; then
    echo "Setting up ~/.git-ai..."
    curl -sSL https://usegitai.com/install.sh | bash
fi

# Build the binary
echo "Building $BUILD_TYPE binary..."
if [[ "$BUILD_TYPE" == "release" ]]; then
    cargo build --release
else
    cargo build
fi

# Install binary via temp file + atomic mv to avoid macOS code signature cache
# issues: direct cp reuses the inode, causing syspolicyd to fail validating the
# changed binary, leaving the process stuck in launched-suspended state unkillably.
echo "Installing binary to ~/.git-ai/bin/git-ai..."
TMP_BIN="$HOME/.git-ai/bin/git-ai.new"
cp "target/$BUILD_TYPE/git-ai" "$TMP_BIN"
mv -f "$TMP_BIN" "$HOME/.git-ai/bin/git-ai"

# Run install hooks
echo "Running install hooks..."
~/.git-ai/bin/git-ai install

echo "Done!"
