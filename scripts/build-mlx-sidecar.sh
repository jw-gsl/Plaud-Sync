#!/bin/bash
# Build the bundled MLX sidecar binary from src-tauri/sidecar/plaud_mlx_transcribe.py
# with PyInstaller, and place it where Plaud Sync's Rust code looks for it:
#
#   src-tauri/binaries/plaud-mlx-transcribe-aarch64-apple-darwin
#
# Run this once per release (Apple Silicon, Python 3.11+) BEFORE the release
# build, then wire `"externalBin": ["binaries/plaud-mlx-transcribe"]` into
# src-tauri/tauri.conf.json → bundle. The 2.3 GB model itself is NOT bundled;
# the helper downloads it to ~/.cache/huggingface on first use (the Settings
# "Download" button runs --warm to do that ahead of the first transcription).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUILD_DIR="$ROOT/build/mlx-sidecar"
OUT="$ROOT/src-tauri/binaries/plaud-mlx-transcribe-aarch64-apple-darwin"
TRIPLE="aarch64-apple-darwin"

rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR" "$ROOT/src-tauri/binaries"

PYTHON="${PYTHON:-python3}"
VENV="$BUILD_DIR/venv"
"$PYTHON" -m venv "$VENV"
# shellcheck disable=SC1091
source "$VENV/bin/activate"
pip install --upgrade pip
pip install "parakeet-mlx>=0.5.0" soundfile pyinstaller

pyinstaller --onefile --target-arch "$TRIPLE" \
  --name plaud-mlx-transcribe \
  --collect-all parakeet_mlx \
  --collect-all mlx \
  --collect-all mlx_lm \
  --hidden-import soundfile \
  --distpath "$BUILD_DIR/dist" \
  "$ROOT/src-tauri/sidecar/plaud_mlx_transcribe.py"

cp "$BUILD_DIR/dist/plaud-mlx-transcribe" "$OUT"
chmod +x "$OUT"
echo "Sidecar ready: $OUT"
echo "Smoke test: $OUT --warm   (downloads the model on first run)"
