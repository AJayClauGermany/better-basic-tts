#!/usr/bin/env python3
"""Example interpreter plugin for Better Basic TTS.

A plugin decides how text is turned into *blocks*, each with a *role*. The
app then reads each block with whatever voice, speed and announcements are
set for its role under Settings -> Voices by role.

This example starts from what the built-in Markdown interpreter produced and
changes three things:

1. Paragraphs that start with "Note:", "Tip:" or "Warning:" get the role
   `callout.note`, `callout.tip`, `callout.warning`. Give `callout` its own
   voice, or `callout.warning` a spoken "Warning" - nothing here needs to
   know which.
2. Python code is described rather than spelled out: `def area(w, h):`
   becomes "Function area, taking w and h."
3. Code in any other language is replaced by a one-line summary.

Use it as a starting point: copy the file, edit `rewrite`, and add it to the
settings file (Settings -> General -> Open file):

    interpreter = "example"

    [[interpreters]]
    id = "example"
    name = "Example plugin"
    program = "python3"
    args = ["/path/to/example_plugin.py"]
    base = "markdown"

Check what it does without listening:

    better-basic-tts explain --file some.md

The protocol is one JSON object in, one out - see README.md next to this
file. Any language works.
"""

import json
import re
import sys

CALLOUT = re.compile(r"^(note|tip|warning)\s*:\s*", re.IGNORECASE)
PY_DEF = re.compile(r"^\s*(?:async\s+)?def\s+(\w+)\s*\(([^)]*)\)")
PY_CLASS = re.compile(r"^\s*class\s+(\w+)")


def describe_python(code):
    """A spoken outline of a Python snippet."""
    lines = []
    for line in code.splitlines():
        if match := PY_DEF.match(line):
            name, args = match.groups()
            args = [a.split(":")[0].split("=")[0].strip() for a in args.split(",") if a.strip()]
            args = [a for a in args if a != "self"]
            taking = f", taking {' and '.join(args)}" if args else ""
            lines.append(f"Function {name.replace('_', ' ')}{taking}.")
        elif match := PY_CLASS.match(line):
            lines.append(f"Class {match.group(1)}.")
        elif line.strip().startswith("#"):
            lines.append("Comment: " + line.strip().lstrip("# ") + ".")
    return "\n".join(lines) or f"{len(code.splitlines())} lines of Python."


def rewrite(block):
    """Return the block (changed or not), or None to drop it."""
    role, text = block["role"], block["text"]

    if role == "paragraph" and (match := CALLOUT.match(text)):
        return {"role": "callout." + match.group(1).lower(), "text": text[match.end():]}

    if role == "code.python":
        return {"role": role, "text": describe_python(text)}

    if role == "code" or role.startswith("code."):
        language = role.partition(".")[2] or "code"
        count = len(text.splitlines())
        return {"role": role, "text": f"{count} lines of {language}."}

    return block


def main():
    request = json.load(sys.stdin)
    # request["text"] is the original text; request["blocks"] is what the
    # built-in interpreter chosen with `base` made of it.
    blocks = [rewrite(block) for block in request["blocks"]]
    json.dump({"blocks": [b for b in blocks if b]}, sys.stdout)


if __name__ == "__main__":
    main()
