#!/usr/bin/env python3
"""KittenTTS adapter for Better Basic TTS (persistent engine protocol).

KittenTTS is a small neural text-to-speech model that runs on the CPU.
Everything happens locally; the model is downloaded once from Hugging Face
and cached.

Setup (a virtual environment keeps it out of your system Python):

    python3 -m venv ~/.local/share/better-basic-tts/kitten
    ~/.local/share/better-basic-tts/kitten/bin/pip install --no-deps kittenml
    ~/.local/share/better-basic-tts/kitten/bin/pip install onnxruntime espeakng_loader phonemizer soundfile numpy huggingface_hub

Then add to the settings file (Settings -> General -> Open file):

    [[engines]]
    id = "kitten"
    name = "KittenTTS"
    program = "/home/YOU/.local/share/better-basic-tts/kitten/bin/python"
    args = ["/path/to/contrib/engines/kitten_tts.py"]
    persistent = true

Optional arguments: --model KittenML/kitten-tts-mini-0.8 (larger, better),
--voice Bella (default voice).

Protocol: one JSON object per line on stdin, one JSON answer per line on
stdout. See crates/core/src/engine/command.rs.
"""

import argparse
import json
import os
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--model", default="KittenML/kitten-tts-nano-0.8-int8")
    parser.add_argument("--voice", default=None, help="default voice")
    args = parser.parse_args()

    # The protocol owns stdout. Libraries (and KittenTTS itself) print
    # progress there, so point the Python-level stdout at stderr and keep the
    # real one for answers.
    answers = os.fdopen(os.dup(sys.stdout.fileno()), "w", encoding="utf-8")
    sys.stdout = sys.stderr

    def answer(obj):
        answers.write(json.dumps(obj) + "\n")
        answers.flush()

    model = None
    voices = []

    def load():
        nonlocal model, voices
        if model is None:
            import soundfile  # noqa: F401  (fail early if missing)
            from kittenml import KittenTTS

            model = KittenTTS(args.model)
            voices = list(getattr(model, "available_voices", []) or [])
        return model

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
            tts = load()
            if request.get("voices"):
                answer({"ok": True, "voices": [{"id": v, "name": v, "language": "en"} for v in voices]})
                continue

            import numpy as np
            import soundfile as sf

            voice = request.get("voice") or args.voice or (voices[0] if voices else None)
            if voices and voice not in voices:
                voice = args.voice if args.voice in voices else voices[0]
            speed = float(request.get("rate") or 1.0)
            kwargs = {"speed": speed}
            if voice:
                kwargs["voice"] = voice
            audio = np.asarray(tts.generate(request["text"], **kwargs), dtype=np.float32)
            audio = audio * float(request.get("volume", 1.0))
            sf.write(request["output"], audio, 24000, subtype="PCM_16")
            answer({"ok": True})
        except Exception as error:  # report, stay alive for the next request
            answer({"ok": False, "error": f"{type(error).__name__}: {error}"})


if __name__ == "__main__":
    main()
