#!/usr/bin/env python3
"""Plaud Sync MLX transcription helper (sidecar).

Transcribes a 16 kHz mono WAV using NVIDIA Parakeet TDT 0.6B v2 via
parakeet-mlx on the Apple GPU (MLX). English audio, Apple Silicon only.

Contract with the Rust side (stdout must stay JSON-only; all diagnostics go
to stderr):
  --warm          Download/load the model once; prints {"ok": true}.
  --audio <wav>   Transcribe; prints
                  {"text": str, "segments": [{"start": s, "end": s, "text": str}]}

Built for bundling with PyInstaller (scripts/build-mlx-sidecar.sh); can also
run from any Python env with parakeet-mlx installed (dev/testing).
"""
from __future__ import annotations

import argparse
import json
import sys
import traceback

MODEL_ID = "mlx-community/parakeet-tdt-0.6b-v2"


def load_model():
    from parakeet_mlx import from_pretrained

    return from_pretrained(MODEL_ID)


def main() -> int:
    parser = argparse.ArgumentParser(prog="plaud-mlx-transcribe")
    parser.add_argument("--audio", help="16 kHz mono WAV to transcribe")
    parser.add_argument(
        "--warm",
        action="store_true",
        help="Only download + load the model (first-run install)",
    )
    args = parser.parse_args()
    if not args.audio and not args.warm:
        parser.error("--audio or --warm is required")

    print(f"loading {MODEL_ID}", file=sys.stderr, flush=True)
    model = load_model()

    if args.warm:
        json.dump({"ok": True, "model": MODEL_ID}, sys.stdout)
        sys.stdout.flush()
        return 0

    print(f"transcribing {args.audio}", file=sys.stderr, flush=True)
    # Chunk long recordings: unchunked, the encoder's attention buffer grows
    # with the square of the audio length and exhausts Metal memory on long
    # recordings, either as a malloc error or an abort inside MLX.
    result = model.transcribe(args.audio, chunk_duration=120, overlap_duration=15)
    payload = {
        "text": result.text.strip(),
        "segments": [
            {"start": float(s.start), "end": float(s.end), "text": s.text.strip()}
            for s in result.sentences
        ],
    }
    json.dump(payload, sys.stdout)
    sys.stdout.flush()
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception:
        traceback.print_exc(file=sys.stderr)
        sys.exit(1)
