# Porting `--help` output (and dynamic completions)

## `ezlo import` first draft

```bash
ezlo import <cmd> && cat ~/.cache/ezlo/generated/<cmd>.ezlo
```

- Probes `<cmd> --help` in a sandbox and scrapes options and subcommands; infers `file`/`dir`/`path` from placeholder names.
- It only reads the top-level help: subcommand flags are often missing or wrong, positionals are never emitted, and there are no generators or recipes. Treat it as a skeleton, then refine it into `~/.config/ezlo/specs/<tool>.ezlo` (generated specs have the lowest precedence and live outside the repo).
- The binary must be installed where ezlo runs; otherwise scrape `--help` manually.

## Manual scraping

```bash
<cmd> --help 2>&1; <cmd> help <sub> 2>&1; <cmd> <sub> --help 2>&1
```

| Help output | ezlo |
| --- | --- |
| `Usage: tool [OPTIONS] <INPUT> [OUTPUT]` | `arg <input>`, `arg [output]` |
| `Usage: tool <COMMAND>` + `Commands:` table | one `cmd tool <sub> "desc"` per row (skip `help`) |
| `-f, --force    Desc` | `opt -f --force "Desc"` |
| `-o, --output <FILE>` / `--output=FILE` / `-o FILE` | `opt -o --output <file> file "..."` |
| `[possible values: a, b, c]` (clap) / `{a,b,c}` (argparse) / `(default "x")` (cobra) | `= a \| b \| c` (for defaults, the default as a single suggestion) |
| `Global Flags:` (cobra) / `global options` | `global` on the root opts |
| `[env: FOO=]` (clap) | `env.FOO` as a param default where relevant |

## Dynamic completion callbacks

Many modern CLIs complete themselves at runtime; wire those into `type` generators instead of hardcoding values:

| Framework | Probe | Generator |
| --- | --- | --- |
| cobra (Go: `kubectl`, `gh`, `docker` plugins, `helm`) | `tool __complete <sub> ''` prints `value\tdesc` lines and a final `:<directive>` line | `$ tool __complete <sub> '' 2>/dev/null \| grep -v '^:'` + `cols value desc` |
| clap_complete dynamic (Rust) | `COMPLETE=bash tool -- tool <sub> ''` | same idea; check the tool's docs for its env var |
| argcomplete (Python) | `_ARGCOMPLETE=1 ...` (awkward) | prefer the tool's own `list` subcommands |

Prefer a dedicated listing command (`tool list --format json`) when it exists: it's faster and gives richer columns than the completion callback.

## Bash completion scripts

`complete -F _tool tool` scripts are imperative (`compgen -W "..."`, `case $prev`), so treat them like help output: harvest `compgen -W` word lists into `= a | b` suggestions, `case $prev in --foo)` branches into opt value types, and `_filedir`/`compgen -f`/`compgen -d` into `file`/`dir`.
