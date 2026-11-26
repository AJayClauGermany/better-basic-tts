# Interpreter plugins

An interpreter decides how a text is read: where the headings, quotes, code
blocks, list items and table cells are. Plain text and Markdown are built
in. A **plugin** changes or replaces that without recompiling the app — it is
any program that reads one JSON object and writes one.

## What you can do without a plugin

Most "read this differently" wishes are settings, not code:

| Wish | Setting |
|------|---------|
| Python and JavaScript code read differently | roles `code.python`, `code.javascript` (Markdown fences name the language) |
| "List" before a list, "End of list" after | `list_item`: *Say when they start* / *Say when they end* |
| "Point" before every item | `list_item`: *Say before each* |
| Top-level headings slower, with a longer pause | role `heading.1` |
| Column headings in another voice | role `table.header` |
| Tables column by column instead of row by row | Settings → Text → Tables |
| Never read code | `code`: *Skip* |

Roles are names with dots for more specific kinds. What is not set for
`code.python` is taken from `code`. The built-in Markdown interpreter
produces:

| Role | For |
|------|-----|
| `heading.1` … `heading.6` | headings by level |
| `paragraph` | paragraphs |
| `quote` | block quotes |
| `list_item` | list items |
| `code`, `code.<language>` | code blocks; the language is the fence's info string |
| `table.header`, `table.cell` | tables read column by column |
| `table.header`, `table.row` | tables read row by row |

A *run* is several blocks in a row whose roles share the part before the
first dot — the items of one list, the headers and cells of one table.
"Say when they start / end" is spoken once per run.

## The protocol

When text is about to be read, the program is started once. It receives on
standard input:

```json
{
  "version": 1,
  "text": "# Title\n\nNote: careful.\n",
  "blocks": [
    {"role": "heading.1", "text": "Title"},
    {"role": "paragraph", "text": "Note: careful."}
  ]
}
```

and answers on standard output:

```json
{"blocks": [
  {"role": "heading.1", "text": "Title"},
  {"role": "callout.note", "text": "careful."}
]}
```

- `text` is the original text. `blocks` is what the built-in interpreter
  named by `base` made of it. Edit `blocks` to tweak the built-in behaviour;
  work from `text` to interpret a format of your own.
- Roles are free-form: lower-case names made of letters, digits, `_`, `-`
  and dots. Invent as many as you like, then give them voices under
  **Settings → Voices by role** ("Add a role").
- Line breaks inside a block's `text` separate sentences that should be
  navigable on their own.
- What you return is still treated as plain text: angle brackets, links and
  your substitutions are handled as always. A plugin cannot hand markup to a
  speech engine.
- If the program exits with an error, takes longer than `timeout_secs`
  (default 10) or prints something else, the built-in interpretation is used
  and the panel says so.

## Installing one

In the settings file (Settings → General → Open file):

```toml
interpreter = "example"

[[interpreters]]
id = "example"
name = "Example plugin"
program = "python3"
args = ["/path/to/contrib/interpreters/example_plugin.py"]
base = "markdown"     # auto | plain | markdown | none
```

It then also appears under **Settings → Text → Read text as**, so you can
switch between it and the built-in interpreters.

Try it without listening:

```bash
better-basic-tts explain --file notes.md
```

[`example_plugin.py`](example_plugin.py) is a small, commented starting
point: it adds `callout.*` roles and describes Python code instead of
spelling it out.
