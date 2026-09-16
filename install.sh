#!/usr/bin/env bash
# jbash: JSON Bourne Again Shell, an AI copilot shell.
# Builds from source and installs to ~/.local/bin (or $JBASH_PREFIX).

set -euo pipefail

NAME="jbash"
PREFIX="${JBASH_PREFIX:-$HOME/.local}"

usage() {
  cat <<EOF
Usage: $0 [options]

  --prefix <dir>   install to <dir>/bin instead of $HOME/.local/bin
  --reconfig       regenerate ~/.jbash_rc if it is missing
  --help           show this help
EOF
}

DORECONFIG=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix) PREFIX="$2"; shift 2 ;;
    --reconfig) DORECONFIG=true; shift ;;
    --help|-h) usage; exit 0 ;;
    *) echo "jbash: unknown option '$1'" >&2; usage >&2; exit 2 ;;
  esac
done

SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BINDIR="$PREFIX/bin"
BIN="$BINDIR/$NAME"

# --- locate the Rust toolchain --------------------------------------------
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
if [[ -x "$CARGO_HOME/bin/cargo" ]]; then
  export PATH="$CARGO_HOME/bin:$PATH"
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "jbash: 'cargo' not found. Install the Rust toolchain first:" >&2
  echo "       curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
  exit 1
fi

# --- build the release binary ----------------------------------------------
echo "jbash: building $NAME (release)…"
( cd "$SRC_DIR" && cargo build --release )

# --- install the binary ----------------------------------------------------
mkdir -p "$BINDIR"
tmp="$BIN.tmp.$$"
cp "$SRC_DIR/target/release/$NAME" "$tmp"
chmod 755 "$tmp"
mv -f "$tmp" "$BIN"
echo "jbash: installed $BIN"

# --- default config --------------------------------------------------------
RC="$HOME/.jbash_rc"
if [[ ! -f "$RC" || "$DORECONFIG" = true ]]; then
  if [[ -f "$RC" ]]; then
    cp "$RC" "$RC.bak"
    echo "jbash: backed up existing $RC to $RC.bak"
  fi
  cat > "$RC" <<'EOF'
# jbash configuration: parameters are read from this file.

# AI server (OpenAI-compatible endpoint, e.g. Ollama)
api_url=http://localhost:11434/v1

# Model served by that endpoint
model=qwen2.5-coder:7b

# Prompt theme: default | minimal | modern
#   default  one line:   git branch, dirty, exec time, venv, error code
#   modern   two lines:  status line above the prompt
#   minimal  name + mode + path only
theme=default

# Confirm before running AI-suggested commands (1/0)
confirm=1

# Send recent conversation turns as AI context (1/0)
context=1

# Request timeout (seconds) and sampling temperature
timeout=120
temp=0.1

# Name shown in the prompt
prompt_name=jbash
EOF
  echo "jbash: wrote default config to $RC (edit it to set your AI endpoint/model)"
fi

# --- PATH reminder ---------------------------------------------------------
case ":$PATH:" in
  *":$BINDIR:"*) ;;
  *) echo "jbash: note: $BINDIR is not on your PATH." >&2
     echo "       add it, e.g. in ~/.bashrc:  export PATH=\"\$HOME/.local/bin:\$PATH\"" >&2 ;;
esac

echo "jbash: done. Run '$NAME' to start the shell, '$NAME --help' for usage."
