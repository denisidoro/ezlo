# CLI Reference

Most of the time you use ezlo through its shell integration. The CLI gives you the same engine for scripting, debugging and spec maintenance. `--cursor` offsets always count characters, not bytes, and default to the end of the line.

| Command | What it does |
|---|---|
| `ezlo init [zsh]` | Print the Zsh integration script (use with `eval`). See [Shell Integration](shell.md). |
| `ezlo complete --line <str> [--cursor <N>] [--cwd <dir>] [--json]` | Run one completion query and print the candidates (TSV, or JSON). |
| `ezlo explain --line <str> [--cursor <N>] [--cwd <dir>] [--json]` | Explain the command under the cursor word by word. See [Line Insight](line-insight.md#explain-alt-h). |
| `ezlo preview --line <str> [--cursor <N>] [--cwd <dir>] [--json]` | Print the effect preview: blast radius and spec dry-runs. See [Line Insight](line-insight.md#effect-preview). |
| `ezlo recall [--cwd <dir>] [--limit <N>] [--json]` | List the commands you run most in a directory and its git repo. See [Learning](learning.md#empty-prompt-recall). |
| `ezlo pick [--line <str>] [--cursor <N>] [--query <text>] [--recipe <id>] [--run] [--rows <N>]` | Open the recipe picker and print the finished command (or run it with `--run`). See [Recipes](recipes.md). |
| `ezlo check [files...]` | Validate specs (all of them by default) and `config.toml`. See [Spec Tooling](spec-tooling.md#ezlo-check-validate-specs). |
| `ezlo check --drift [<cmd>...]` | Compare specs with each tool's live `--help`. See [Spec Tooling](spec-tooling.md#ezlo-check---drift-keep-specs-up-to-date). |
| `ezlo parse <file>` | Print a spec's parsed form as JSON. |
| `ezlo import <cmd>` | Draft a spec from `<cmd> --help` (sandboxed) into `~/.cache/ezlo/generated/<cmd>.ezlo`. |
| `ezlo history import [<path>]` | Import Zsh history (default `$HISTFILE` or the first existing default file) into ezlo's database. |
| `ezlo history search [--query <text>] [--histfile <path>] [--ignore-space] [--rows <N>]` | Search your history interactively and print the picked line (what `Ctrl-R` runs). See [Learning](learning.md#history-search-ctrl-r). |
| `ezlo daemon [--detach] [--status] [--stop] [--socket <path>]` | Manage the background daemon (started automatically by the Zsh hook). |

## Examples

```sh
# What would ezlo suggest here?
ezlo complete --line 'git checkout ' --json

# Why is this line flagged, and where do suggestions come from?
ezlo explain --line 'docker logs --tial 10 web'

# How much would this delete?
ezlo preview --line 'rm -rf build/ *.log'

# Is my spec still in sync with the installed tool?
ezlo check --drift docker
```
