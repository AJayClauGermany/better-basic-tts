#!/usr/bin/env python3
"""NeuTTS adapter for Better Basic TTS (persistent engine protocol).

NeuTTS (by Neuphonic) is an on-device neural text-to-speech model that clones
a voice from a short reference recording. Everything happens locally.

NOTE: this adapter was written from the NeuTTS documentation and has not been
run against the real model by the authors of Better Basic TTS. Please report
what needs adjusting.

Setup:

    sudo apt install espeak-ng        # macOS: brew install espeak-ng
    python3 -m venv ~/.local/share/better-basic-tts/neutts
    ~/.local/share/better-basic-tts/neutts/bin/pip install "neutts[all]" soundfile

Voices are reference recordings: a folder with pairs of files such as
`dave.wav` + `dave.txt` (a clean 3-15 second mono clip and its transcript).
The NeuTTS repository ships some in its `samples/` folder.

Settings file entry:

    [[engines]]
    id = "neutts"
    name = "NeuTTS"
    program = "/home/YOU/.local/share/better-basic-tts/neutts/bin/python"
    args = ["/path/to/contrib/engines/neutts.py", "--voices-dir", "/path/to/neutts/samples"]
    persistent = true

NeuTTS has no speed control, so the adapter time-stretches nothing: the
`rate` of a request is ignored and the turtle / rabbit have no effect with
this engine.
"""

import argparse
import json
import os
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--voices-dir", required=True, help="folder with NAME.wav + NAME.txt pairs")
    parser.add_argument("--backbone", default="neuphonic/neutts-nano-q4-gguf")
    parser.add_argument("--codec", default="neuphonic/neucodec")
    parser.add_argument("--device", default="cpu")
    args = parser.parse_args()

    answers = os.fdopen(os.dup(sys.stdout.fileno()), "w", encoding="utf-8")
    sys.stdout = sys.stderr

    def answer(obj):
        answers.write(json.dumps(obj) + "\n")
        answers.flush()

    def list_voices():
        names = []
        for name in sorted(os.listdir(args.voices_dir)):
            stem, ext = os.path.splitext(name)
            if ext.lower() == ".wav" and os.path.exists(os.path.join(args.voices_dir, stem + ".txt")):
                names.append(stem)
        return names

    model = None
    references = {}  # voice -> (codes, transcript)

    def load():
        nonlocal model
        if model is None:
            from neutts import NeuTTS

            model = NeuTTS(
                backbone_repo=args.backbone,
                backbone_device=args.device,
                codec_repo=args.codec,
                codec_device=args.device,
            )
        return model

    def reference(tts, voice):
        if voice not in references:
            wav = os.path.join(args.voices_dir, voice + ".wav")
            with open(os.path.join(args.voices_dir, voice + ".txt"), encoding="utf-8") as f:
                transcript = f.read().strip()
            references[voice] = (tts.encode_reference(wav), transcript)
        return references[voice]

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
            voices = list_voices()
            if request.get("voices"):
                answer({"ok": True, "voices": [{"id": v, "name": v.capitalize()} for v in voices]})
                continue
            if not voices:
                raise RuntimeError(f"no NAME.wav + NAME.txt pairs in {args.voices_dir}")

            import numpy as np
            import soundfile as sf

            voice = request.get("voice")
            if voice not in voices:
                voice = voices[0]
            tts = load()
            codes, transcript = reference(tts, voice)
            audio = np.asarray(tts.infer(request["text"], codes, transcript), dtype=np.float32)
            audio = audio.reshape(-1) * float(request.get("volume", 1.0))
            sf.write(request["output"], audio, 24000, subtype="PCM_16")
            answer({"ok": True})
        except Exception as error:
            answer({"ok": False, "error": f"{type(error).__name__}: {error}"})


if __name__ == "__main__":
    main()
