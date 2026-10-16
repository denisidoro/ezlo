# Learning

ezlo records what you accept from the dropdown and what you run (with its directory and exit code), and uses that to rank and suggest. All of it stays on your machine, in `~/.local/share/ezlo/ezlo.db`.

---

## Usage memory

- **Ranking**: every candidate's score mixes how often you used it (accepts count more than executions) with how recently, decaying over time. Tokens used in the current directory get a bonus.
- **Project-scoped**: "the current directory" means its enclosing git repository, so moving between subfolders of a repo keeps that project's memory: rankings, learned values and ghost text.
- **Learned candidates**: what you typed before becomes a suggestion, even where no spec defines values:
  - an argument with no declared type,
  - an option value with no type,
  - arguments and flags of commands ezlo has no spec for.

  ```text
  $ make dep█        → deploy-staging  (learned from `make deploy-staging` runs)
  ```

### Seeding from your history

The Zsh dropdown reads `$HISTFILE` live for history rows and ghost text. To also seed ezlo's own database (which tracks directories and exit codes), import your history once:

```sh
ezlo history import            # $HISTFILE, ~/.zsh_history or ~/.histfile
ezlo history import ~/old_hist # a specific zsh history file
```

### History rows

History rows in the dropdown carry three columns on the right:

- **Where**: `here` when the line ran in this directory or repo; else the project kind it shares with this directory (`rust`, `node`, `go`, `python`, `bazel`, `docker`, …, detected from `Cargo.toml`, `package.json`, `go.mod`, … up to the repo root).
- **Last run**: `✗ 2` (red) when its most recent run failed, else how long it took (`1.5s`, `3m05s`) when that was a second or more.
- **Age** of the run.

Ranking follows the same order: this directory, its repo, **projects of the same kind**, then anywhere. In a fresh Rust crate, `cargo nextest run` from your other crates comes before a newer, unrelated line.

---

## Failure penalties

A line that exits with an error counts its words as **failures** instead of uses:

- From the second failure on, a candidate's score drops in proportion to how often it fails, so chronically failing commands and flags sink below ones that work.
- Words that have never succeeded aren't offered as learned values at all.
- Interruptions (`Ctrl-C`, `Ctrl-\`, `Ctrl-Z`, SIGPIPE, SIGKILL/SIGTERM) don't count as failures; any other non-zero exit does.

---

## Empty-prompt recall

Press `Tab` (or `Ctrl-Space`) on an empty prompt to see what you usually run here:

```text
$ █
  ▸  cargo test -p ezlo --offline
     make deploy
     git status
```

- Lines are ranked by frequency and recency of successful runs in the current directory first, then elsewhere in the same git repo, then in projects of the same kind (`rust`, `node`, …), then anywhere (weighted much lower), topped up with your newest history lines.
- `ezlo recall` prints the same list from the CLI.
- Keep `Tab` on an empty prompt for its previous binding with `zstyle ':ezlo:recall' empty-tab no` (`Ctrl-Space` still recalls).

## History search (`Ctrl-R`)

`Ctrl-R` replaces zsh's incremental search with an inline picker over your whole history, seeded with what's on the line:

```text
$ git push█                               ← press Ctrl-R
 ~/src/app · 2h ago · exit 1
 history ❯ git push                                  1/3
┌──────────────────────────────────────────────────────┐
│▸ git push --force-with-lease origin main    ✗ 2h ago │
│  git push origin feature/login                1d ago │
│  git push -u origin HEAD                      3d ago │
└──────────────────────────────────────────────────────┘
```

- **Sources**: ezlo's own log of what you ran (with the directory and exit status) merged with `$HISTFILE`, one row per unique line (trailing spaces ignored), newest first. Lines that last failed get a red `✗`.
- **Matching**: space-separated terms, all of which must appear, in any order (`push force` finds `git push --force`). Case is ignored unless a term has an uppercase letter.
- **Keys**: type to filter; `↑`/`↓` (or `Ctrl-P`/`Ctrl-N`, `Tab`), `PageUp`/`PageDown` move; `Ctrl-R` cycles the scope between everything, this git repo and this directory; `Enter` puts the line on the prompt without running it, `Ctrl-O` (or `Alt-Enter`) runs it right away; `Esc`, `Ctrl-C` or `Ctrl-G` cancel and keep what you had typed.
- The picker takes ~60% of the terminal height (`--rows N` overrides it). Lines are colored like in `ezlo pick` (command bold blue, flags cyan, operators magenta, quotes green), with the query terms underlined in yellow. `ezlo history search` runs the same picker from the CLI and prints the picked line.
- To keep your own `Ctrl-R` (e.g. fzf or atuin), set `zstyle ':ezlo:keys' search ''` before loading ezlo, or bind the `_ezlo_search` widget to another key.
