# Completion

ezlo's main surface is a dropdown that appears below your Zsh prompt as you type. This page covers what it shows and the contexts where ezlo completes something even without a spec.

---

## The dropdown

```text
$ git rebase █
  ▸  main            fix: handle empty config     2 days ago      ← rows
     feature/login   wip: login form              5 hours ago
  git rebase  [upstream]  [branch]                        1/12    ← status line
```

- **Suggestions appear as you type**: after the first space, or once you've typed `[complete] min_chars` characters (default 3) of a command name.
- **Rows are fuzzy-matched and ranked** by how well they match plus how often and how recently you used them, in this directory or git repo (see [Learning](learning.md)).
- **Nothing is inserted until you accept**: `Tab` accepts the selected row, `Up`/`Down` move, `Esc` dismisses, and `Enter` always runs the line exactly as typed.
- **Rows carry context**: Nerd Font icons per kind and file type, colors (folders blue, executables green, flags yellow, …), descriptions, and aligned data columns from specs (a branch's last commit subject and age, a container's state and image). Status words are colored semantically: `running` green, `paused` yellow, `exited` dim, `failed` red.
- **Files and folders** show their size or item count and age.
- **Matching history lines** are mixed in (up to `[complete.dropdown] history_rows`, default 3), so a long command you ran before is one `Tab` away.
- **One status line at most** below the rows, so metadata never costs more than one suggestion (see [Status line](#status-line)).
- **Tune it per shell** with `Alt-K`: a [settings popup](shell.md#settings-popup-alt-k) turns suggestions off, changes the number of rows, history rows and minimum chars, and switches ghost text, the signature hint, diagnostics and previews, without touching your config.

### Layout details

- Long labels don't push everything else around: description columns are sized per group (flags, subcommands, recipes, values), and a single outlier label doesn't widen its group's column.
- Data columns (file size and age, history, a spec's `show` columns) are sized per kind of row too, so history rows aren't squeezed by containers with long image names. A row whose label is too long for its group's columns gets columns sized to its own values (only that row loses alignment).
- Long paths and identifiers are cut in the middle, keeping the part that differs: `services/back…/user_profile.go`, `feature/JIRA-…-logic`. When the selected row was cut, its full text shows in the status line (or on top of the preview pane).
- When there are more rows than fit, a thin `┃` scrollbar thumb on the right edge shows where you are.
- The dropdown is drawn with `POSTDISPLAY`, so it never scrolls your terminal or leaves traces in scrollback.

### Status line

The line under the rows shows the most important of these, in this order, with the scroll position (`3/40`) on its right (and, when the preview pane overflows, its position: `pane 1-5/40 ⇅PgUp/PgDn`). When there is nothing to say, there is no line.

| Priority | Content | Style |
|---|---|---|
| 1 | A diagnostic for the line (`⚠ unknown option '--bogus'`) | red / yellow |
| 2 | An effect warning (`⚠ rm: 1 200 files`) | yellow |
| 3 | The selected row's full text, only when the row was truncated (on top of the preview pane instead, when it is open beside the rows) | cyan |
| 4 | The [signature hint](#positional-signature-hint) | gray |

[Effect previews](line-insight.md#effect-preview) (`⇢ rm: 2 files · 300 B`, `= /home/you`, dry-run output) are drawn below the status line, even with the dropdown open (up to 2 lines, `zstyle ':ezlo:preview' max-lines`); their warnings go to the status line.

---

## Ghost text

When the line so far is the start of something in your history, the rest is shown in dim text after the cursor (like fish):

```text
$ cargo test -p ki█te --offline
```

- `→` or `End` at the end of the line accepts the whole suggestion; `Alt-→` / `Alt-F` accepts one word.
- `[complete.ghost] mode` controls it: `auto` (default; hidden while the dropdown is open), `on`, or `off`.

---

## Positional signature hint

While you fill positional arguments of a command that has a spec and **at least two positionals**, ezlo shows its signature in the [status line](#status-line) and underlines the slot you're on (for a single positional the dropdown already says it all):

```text
$ scp notes.txt █
  scp  <source…>  [target]
```

- `<name>` is required, `[name]` optional, `…` repeatable.
- Options and their values don't count as positions, and `--` is respected.
- Turn it off with `[complete] signature = false`.

---

## Preview pane (`Alt-P`)

The selected row can show a small preview beside the dropdown, without opening `ezlo pick`:

```text
$ git checkout █
  ▸  main            2 days ago     │ a1b2c3d fix: handle empty config
     feature/login   5 hours ago    │ 9f8e7d6 feat: add config loader
                                    │ 4c5d6e7 chore: bump deps
```

- **Type previews**: a spec type's `preview` command (e.g. `git log` for a branch, `docker logs` for a container).
- **Path previews**: the first lines of a text file, or a compact directory listing (`ls`, `cd`, `cat`, …). With `bat` (or `batcat`) / `eza` installed, they render them, colors included (`[complete.pane] file_commands` / `dir_commands`); empty, binary and unreadable files keep the built-in note.
- **When it opens by itself** (`mode = "auto"`): only when it costs no suggestion lines, i.e. when it fits beside the rows *and* most rows (history rows aside) have a preview. Type and path previews follow the same rule. Otherwise (narrow terminal, a lone previewable row among flags) it waits for `Alt-P`.
- `Alt-P` toggles the pane for the rest of the shell session (the [settings popup](shell.md#settings-popup-alt-k) can also set it to auto / on / off). `[complete.pane]` controls the mode, height, position, width and timeout (see [Configuration](configuration.md)).
- **Placement**: beside the rows, which are laid out narrower (the pane takes 40% of the terminal by default); the pane is as tall as the dropdown (at least `lines`), and continues below a shorter one, but never gets taller than the dropdown's maximum rows (beside or below). On narrow terminals (or with `position = "bottom"`) an `Alt-P` pane goes below the dropdown instead.
- **Scrolling**: `Shift-Up` / `Shift-Down` (or `PageUp` / `PageDown`, while a pane is shown) scroll a preview longer than the pane by half its height (a `┃` thumb on the gutter shows where you are); another row starts at its top. Up to 200 lines are kept.
- Previews are computed in the background: typing never waits for them, and the previous preview stays on screen until the next one is ready, so the layout doesn't jump.

---

## Comment lines: natural language to command

Start the line with `#` and describe what you want. Instead of completions, the dropdown offers a single row that turns the text into a real command:

```text
$ # list files by size, biggest first█
  ▸  Translate to command
  Tab: ask `dot llm chat zsh`
```

- `Tab` on that row (or `Ctrl-X Ctrl-L` on any line) sends the text to your translator and shows `⋯ translating… (esc cancels)` below the prompt. When the answer arrives, each suggested command becomes a dropdown row (usually one, a few when the request is ambiguous); the comment stays on the line. `Tab` replaces the line with the selected command (undo, `Ctrl-_`, brings the comment back); `Esc`, `Enter` or editing the line ignores them.
- It runs in the background: you can keep editing. Editing the line or pressing `Esc` cancels it. Failures (non-zero exit, empty output, timeout) show `✗ translate: …` and leave the comment as it was.
- Nothing runs while you type: the translator is only called when you accept the row.
- **ezlo has no AI of its own.** The translator is any program you set in `[complete.translate] command` (see [Configuration](configuration.md)): it gets the text (without the `#`) on stdin and prints one or more one-liners on stdout, one per line. Markdown code fences, blank and `#` comment lines and duplicates are dropped, and backticks around a line and a leading `$ ` are stripped (at most 10 suggestions). It runs in the shell's directory and exported environment, so programs on your `PATH` resolve as in the shell.
- With no translator configured (the default), comment lines simply get no suggestions.

A minimal translator could look like this (`llm` stands for whatever CLI talks to your model):

```sh
#!/bin/sh
# ~/bin/nl2cmd: request on stdin, zsh one-liners on stdout (one per line)
llm -s "Reply with zsh one-liners, one per line, no explanations. Prefer one; add alternatives only if the request is ambiguous." "$(cat)"
```

---

## Where suggestions come from

For each word, ezlo figures out what is expected there (a subcommand, a flag, a flag's value, a positional argument) and asks the matching source:

1. **Specs**: [`.ezlo` files](specs.md) describe a command's subcommands, flags and arguments, and where argument values come from (a shell command, a fixed list, or a built-in type like `file`, `pid` or `host`). User specs, bundled specs (`git`, common file tools) and generated ones are layered (see [Spec Tooling](spec-tooling.md)).
2. **Learned values**: tokens you've used before in the same position, even for commands without a spec (see [Learning](learning.md)).
3. **History**: whole lines from `$HISTFILE`.
4. **Native zsh completion**: for commands no spec knows, a background `zsh -f` worker runs zsh's own completion system, so you still get its suggestions in the dropdown.

`Alt-H` shows which of these sources produced the current suggestions (see [Line Insight](line-insight.md#explain-alt-h)).

---

## Contexts that work without a spec

### Environment assignments

```text
$ RUST_L█           → RUST_LOG=
$ export PATH=$PATH:~/.lo█     → completes the path segment after the last ':'
```

- An upper-case word at the start of a command, or an `export` argument, offers the variables of your shell, with their current values as descriptions.
- After `NAME=`, the current value comes first, then paths. Colon lists (`PATH`, `MANPATH`, anything ending in `PATH`) complete only the segment under the cursor.

### Parameters (`$VAR`)

```text
$ cd $XDG_CO█       → $XDG_CONFIG_HOME   /home/you/.config
$ echo ${HO█        → ${HOME}
```

- A `$` followed by a few letters anywhere in a word completes your shell's variable names, for every command (with or without a spec), with their values as descriptions. After `${`, the closing `}` is added.
- Not inside single quotes or after `\$`. `$((1 + 2))` is arithmetic, not a command substitution.

### Nested command lines

Wrappers like `sudo`, `env`, `time`, `nice`, `nohup`, `xargs` (configurable via `[line] wrappers`), and any spec argument of type `cmd` (`timeout`, `watch`, `ssh host <command…>`), start a new command line inside the current one:

```text
$ sudo timeout 5 git checkout --f█     → git checkout flags
```

The inner command gets full support: its subcommands, flags, values, learned usage and diagnostics. This nests as deep as you need.

### Pipelines

When a stage reads data from a file, ezlo samples that file and suggests things that make sense for the receiving tool:

```text
$ cat users.json | jq '.us█        → .users  .users[]  .users[].name  .users[].email
$ grep -e ER█ app.log              → ERROR (2× in app.log)
$ cut -d, -f█ people.csv           → 1 (name)  2 (age)  3 (city)
$ awk -F█ people.csv               → ,  (comma: 3 columns (name, age, city))
```

- **Supported tools**: `jq` (plus `gojq`, `jaq`), `grep`/`rg`, `awk`, and `cut`.
- **Data sources**: an input redirect (`jq . < f`), the tool's own file argument (`jq .x f.json`), or an upstream `cat`/`head`/`tail`/`sort`/`uniq`/`bat` of a file, followed through pass-through filters.
- **Safe by design**: ezlo never runs the upstream command. It only reads a regular file with a literal path, and at most its first 1 MiB. `curl … | jq` gets no data suggestions.
