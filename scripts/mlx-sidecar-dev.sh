#!/bin/sh
# Dev-time stand-in for the bundled MLX sidecar binary: runs the entry script
# with an existing Python env that has parakeet-mlx installed.
#
#   export PLAUD_MLX_SIDECAR="$PWD/scripts/mlx-sidecar-dev.sh"
#
# Uses the HiDock pipeline venv by default; point PLAUD_MLX_PYTHON at any
# env with parakeet-mlx installed.
set -e
DIR="$(cd "$(dirname "$0")" && pwd)"
PYTHON="${PLAUD_MLX_PYTHON:-/Users/jameswhiting/_git/hidock-tools/transcription-pipeline/.venv/bin/python3}"
exec "$PYTHON" "$DIR/../src-tauri/sidecar/plaud_mlx_transcribe.py" "$@"
