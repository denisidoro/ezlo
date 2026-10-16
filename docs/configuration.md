# Configuration

ezlo works without configuration. To change its behavior, create `$XDG_CONFIG_HOME/ezlo/config.toml` (default `~/.config/ezlo/config.toml`). Every key below is optional and shown with its default.

```toml
[sources]                     # where candidates come from
enabled = ["user", "bundled", "native", "generated"]
# Spec precedence is fixed: user > bundled > generated.
# "native" (zsh's own completion) is only used for commands no spec knows.

[sources.user]
dirs = []                     # user spec dirs (default: $EZLO_SPECS_DIR or ~/.config/ezlo/specs)

[sources.native]
rc = []                       # files sourced in the native zsh worker (e.g. to load completion plugins)

[line]                        # how a typed line is interpreted
wrappers = ["sudo", "env", "time", "nice", "nohup", "command", "exec", "xargs"]
                              # words that start a nested command line

[generators]                  # spec `$` commands (completion and ezlo pick)
timeout = "1s"                # killed after this long

[complete]                    # inline Zsh completion
min_chars = 3                 # chars of a command name typed before suggesting (0: suggest immediately)
signature = true              # positional signature hint

[complete.dropdown]
rows = 6                      # visible rows ($EZLO_MAX_ROWS overrides; capped to the terminal)
history_rows = 3              # history lines mixed into spec suggestions (0 disables)

[complete.ghost]
mode = "auto"                 # "auto" | "on" | "off"

[complete.pane]               # preview pane of the selected row (Alt-P toggles)
mode = "auto"                 # "auto": opens by itself when it fits beside the rows and most rows have a preview | "on": always | "off": Alt-P only
lines = 5                     # visible height (beside the rows: at least the dropdown's); longer previews scroll (Shift-Up/Down)
paths = true                  # built-in file head / directory listing previews
timeout = "500ms"             # type preview command timeout
position = "right"            # "right": beside the dropdown rows (below them when the terminal is too narrow) | "bottom"
width = 40                    # right-hand pane width, % of the terminal
file_commands = ["bat --color=always --style=plain --paging=never --wrap=never --line-range=:200 \"$path\"", "batcat …"]  # text file previews: first whose program is on $PATH runs ($path set, colors kept); none / []: built-in head
dir_commands = ["eza --color=always --all --group-directories-first --grid --width=\"$width\" \"$path\""]  # directory previews ($path, $width set); none / []: built-in listing

[complete.translate]          # comment lines (`# list files by size`) → command; see completion.md
command = []                  # argv of your translator (no shell): text on stdin, one-liners on stdout (one per line); [] disables
timeout = "20s"               # kill timeout

[pick]                        # recipe picker
rows = 0                      # height; 0 = about 40% of the terminal

[preview]                     # effect preview while editing
enabled = true
commands = ["rm", "rmdir", "mv", "cp", "chmod", "chown", "chgrp"]  # blast-radius commands ([] disables)
dry_run = true                # run spec `preview $ …` dry-runs
safety = true                 # warn about risky lines: rm -rf ~/*, unset $VAR in rm operands, > or cp/mv overwriting files, git push --force
instant = ["echo", "pwd", "cd", "mkdir", …]  # in-process previews: values of read-only commands (`echo $USER`), cd/mkdir/touch/ln effects ([] disables; default: every supported command)
timeout = "500ms"             # dry-run timeout
max_entries = 20000           # filesystem entries scanned per preview
```

- **Invalid values** make ezlo ignore the whole file (and fall back to defaults).
- **Unknown keys** are ignored, but reported by `ezlo check` and in the daemon log, so typos don't go unnoticed.

Keybindings and colors are configured with `zstyle` instead (see [Shell Integration & Keys](shell.md)). To try a value in one shell without editing this file, use the `Alt-K` [settings popup](shell.md#settings-popup-alt-k): it overrides `rows`, `history_rows`, `min_chars`, `signature` and `[preview] enabled` for that shell only.

---

## Environment variables

| Variable | Purpose |
|---|---|
| `EZLO_SPECS_DIR` | User spec directories, colon-separated (when `[sources.user] dirs` is empty) |
| `EZLO_CONFIG_DIR` | Config directory (default `$XDG_CONFIG_HOME/ezlo`) |
| `EZLO_CACHE_DIR` | Cache directory: generated specs, spec index (default `~/.cache/ezlo`) |
| `EZLO_DATA_DIR` | Data directory: the usage database `ezlo.db` (default `$XDG_DATA_HOME/ezlo`, i.e. `~/.local/share/ezlo`) |
| `EZLO_SOCKET` | Daemon socket path (default `$XDG_RUNTIME_DIR/ezlo.sock`, else `$TMPDIR/ezlo-<uid>/ezlo.sock`) |
| `EZLO_MAX_ROWS` | Dropdown rows, overriding `[complete.dropdown] rows` (the `Alt-K` popup's value wins over both) |
