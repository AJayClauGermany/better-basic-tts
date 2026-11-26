# Engine adapters

Better Basic TTS can use any local program as a speech engine. Small, fast
programs (eSpeak, Piper) are simply started once per sentence — see the main
README. Neural models take seconds to load, so for those there is a
**persistent** mode: the program is started once and then receives requests
over a tiny line protocol.

## The protocol

One JSON object per line on the program's standard input; one JSON answer per
line on its standard output.

```text
→ {"text": "Hello.", "voice": "Luna", "rate": 1.2, "pitch": 1.0, "volume": 1.0, "output": "/tmp/x.wav"}
← {"ok": true}

→ {"voices": true}
← {"ok": true, "voices": [{"id": "Luna", "name": "Luna", "language": "en"}]}

← {"ok": false, "error": "what went wrong"}
```

- `text` is plain text, one sentence at a time, already cleaned up (no angle
  brackets, no markup). Never interpret it as markup.
- Write a WAV file (any common PCM format, mono or stereo) to `output`, then
  answer.
- `rate` is a speed multiplier (1.0 = normal). Ignore what you cannot do.
- Lines on stdout that do not start with `{` are ignored, and stderr is kept
  for error messages, so libraries may print freely.
- If the program exits, it is restarted on the next request.
- The `voices` request is optional; voices listed in the settings win.

Settings entry:

```toml
[[engines]]
id = "kitten"
name = "KittenTTS"
program = "/home/you/.local/share/better-basic-tts/kitten/bin/python"
args = ["/path/to/contrib/engines/kitten_tts.py"]
persistent = true
```

Then pick it under **Settings → Voice → Engine**, or set `engine = "kitten"`.

## Included adapters

| File | Engine | Status |
|------|--------|--------|
| [`kitten_tts.py`](kitten_tts.py) | [KittenTTS](https://github.com/KittenML/KittenTTS) — 15–80 MB models, CPU, 8 voices | tested on Linux with `kitten-tts-nano-0.8-int8` |
| [`neutts.py`](neutts.py) | [NeuTTS](https://github.com/neuphonic/neutts-air) — voice cloning from a reference clip | written from the documentation, **not yet run** against the model |

Setup instructions are at the top of each file. Both download their model
from Hugging Face on first use and work offline afterwards; speech synthesis
itself never leaves the machine.
