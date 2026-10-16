# Porting man pages

Man pages are the most complete flag list but the least structured. Use them to check coverage and descriptions, and a structured source (Fig, fish, zsh) for the grammar when one exists.

## Getting plain text

```bash
MANWIDTH=200 man <cmd> 2>/dev/null | col -bx        # rendered, no overstrike
man -w <cmd>                                         # path to the roff source
mandoc -T utf8 "$(man -w <cmd>)" | col -bx           # BSD/macOS alternative
man <section> <cmd>                                   # e.g. `man 1 kill` vs `man 2 kill`
```

- macOS ships BSD man pages and Linux ships GNU/procps ones: flags differ (`sed -i`, `ps`, `date`, `stat`, `find -printf`). Read both when the spec must work on both, and keep only common flags or note the platform in the desc ("Linux only").
- `man <cmd>-<sub>` (`git-commit`, `docker-run`) often exists for subcommands.

## Mapping

| Man section | ezlo |
| --- | --- |
| `NAME` (`cmd - one line`) | `cmd <tool> "One line"` (sentence case, no trailing period) |
| `SYNOPSIS` | positional `arg` lines: `FILE...` → `<files...>`, `[FILE]` → `[file]`, `[FILE...]` → `[files...]`; also reveals subcommands and mutually exclusive groups (`[-a \| -b]` → `excludes`) |
| `OPTIONS` / `DESCRIPTION` item `-f, --force` | `opt -f --force "first sentence"` |
| `-o FILE`, `--output=FILE`, `--output FILE`, `-oFILE` | `opt -o --output <file> file "..."` |
| `--color[=WHEN]` (optional value) | `opt --color <when> = always \| never \| auto "..."` (ezlo has no optional-value flags; the value is completed when given) |
| `WHEN is 'always', 'never', or 'auto'` in the item body | `= always \| never \| auto` suggestions, or a `values` type if reused |
| `-v` "may be repeated" | plain `opt -v` (repeat works without modeling) |
| `COMMANDS` / `SUBCOMMANDS` | one `cmd <tool> <sub>` each |
| `ENVIRONMENT` | `env.VAR` defaults for type params (e.g. `env.DOCKER_HOST`) |
| `EXAMPLES` | candidate `recipe`s (only non-trivial ones) |
| `FILES` | hints for generators (config files to parse for hosts, profiles, etc.) |

## Value placeholders → types

- `FILE`, `PATH`, `CONFIG` → `file` / `path`; `DIR`, `DIRECTORY` → `dir` (this is what `ezlo import` infers too).
- `PID` → `process`, `USER` → a user type, `SIGNAL` → `signal` (copy from an existing spec that declares it, if any).
- `NUM`, `SIZE`, `DURATION`, free text → untyped, optionally with `= ` example suggestions.
- Anything listable by the tool itself (`--list-*`, `list` subcommand) → a `$` generator type.

## Gotchas

- Wrapped descriptions span several indented lines: join them and keep only the first sentence (ezlo rows are one line).
- Hyphenated words broken across lines (`proc-\n ess`) and `‐` (U+2010) hyphens in flags (`‐‐force`) appear in some renderings: normalize to ASCII `-`.
- Deprecated/obsolete flags: skip unless still common.
- Flags listed only in `SYNOPSIS` or only in prose: cross-check with `<cmd> --help`.
