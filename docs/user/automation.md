# Automation and agents

Everything you can do in PdfCraft can also be done without the window: from scripts, from other programs, and by AI agents you choose to connect. None of this is switched on unless you start it yourself.

## The command-line tool

`pdfcraft-cli` is a download of its own, next to the app on the releases page, for your computer or for servers.

```sh
pdfcraft-cli info form.pdf                       # what is in a file, as JSON
pdfcraft-cli text paper.pdf --page 3             # the text of a page, in reading order
pdfcraft-cli edit in.pdf --rotate 1,2:90 --delete 5 --title "Q3" --out out.pdf
```

`pdfcraft-cli tools` lists every tool with a description of its arguments. `pdfcraft-cli run` calls one, or a whole script of steps:

```sh
pdfcraft-cli run text_find doc=1 query=invoice
pdfcraft-cli run --script steps.json --root ./work
```

With `--root`, every file a script reads or writes must be inside that folder.

## Tools for every feature

Each feature in this guide has a tool: opening and saving, rendering pages to images, finding text, page changes, comments, forms, signatures, redaction, OCR, exports, preferences and more. A few examples:

- `help_shortcuts` returns the keyboard shortcuts, the same list as Help → Keyboard shortcuts. `help_guide` lists the pages of this guide, returns one, or searches them.
- `command_list` lists every command in the menus with its shortcut, whether it can run now, and the tool that runs it.
- `prefs_get` and `prefs_set` read and change [Preferences](preferences.md).

Changes stay in memory, and can be undone, until a `doc_save` step saves them. Saving to the same file adds to it, as in the app. Unsaved work is never thrown away without asking.

## Connecting an AI agent (MCP)

`pdfcraft-cli mcp` runs a Model Context Protocol server, which lets an AI agent use the same tools. PdfCraft never starts it on its own and it opens no network port: your agent starts it and talks to it directly, and it stops when the agent disconnects. Add it to your agent's configuration, for example:

```json
{ "mcpServers": { "pdfcraft": { "command": "pdfcraft-cli", "args": ["mcp", "--root", "/path/to/your/pdfs"] } } }
```

- `--root` keeps the agent inside one folder.
- `--compact` gives the agent a short list of core tools plus a search, so it spends less effort reading tool descriptions. Every tool still works.

## Letting an agent drive the app

Started as `pdfcraft --control ~/.pdfcraft-control.json`, the desktop app accepts commands from programs on the same computer: read the window's controls, click, type, press keys, run commands, change view settings and take screenshots. It is off unless you start the app this way. It listens only on your own computer, and every connection needs a secret written to that file, which only you can read. Keep the file in a folder only you can write to.

```sh
pdfcraft-cli ui --control ~/.pdfcraft-control.json command id=help.shortcuts
pdfcraft-cli ui --control ~/.pdfcraft-control.json screenshot --out window.png
```

## Custom commands and guided actions

**Custom commands** save a tool with its settings under a name of your own, so you can run it again in one step. **Guided actions** run several steps over many files (see [Creating, converting and printing](create-and-export.md)).

More detail for developers is in the project's `docs/mcp.md` and `crates/automation/README.md`.
