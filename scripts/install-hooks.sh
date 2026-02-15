#!/bin/bash
# Install git hooks for diesel-sqlite-session

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
HOOKS_DIR="$REPO_ROOT/.git/hooks"

echo "Installing git hooks..."

# Install pre-commit hook
cp "$SCRIPT_DIR/pre-commit" "$HOOKS_DIR/pre-commit"
chmod +x "$HOOKS_DIR/pre-commit"
echo "Installed pre-commit hook"

# Install pre-push hook
cp "$SCRIPT_DIR/pre-push" "$HOOKS_DIR/pre-push"
chmod +x "$HOOKS_DIR/pre-push"
echo "Installed pre-push hook"

echo "Git hooks installed successfully!"
echo ""
echo "Alternatively, you can use the pre-commit framework:"
echo "  pip install pre-commit"
echo "  pre-commit install"
