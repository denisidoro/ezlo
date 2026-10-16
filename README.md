# ezlo 🧢

**ezlo is a talking hat for your terminal.** It perches on top of your zsh prompt and, as you type, it shows a live completion dropdown, explains and sanity-checks the line before you hit Enter, learns from what you actually run, and turns common chores into fill-in-the-blanks recipes.

[![Demo](examples/demo.svg)](https://asciinema.org/a/KfHtI0wivSFlcSGD)

Everything it knows about a command comes from small declarative `.ezlo` files (plus your shell history and, for unknown commands, zsh's own completion system), so teaching it a new tool is a few lines of text, not a plugin.

```text
$ git checkout ma█
  ▸  main            fix: handle empty config     2 days ago
     master-old      chore: bump deps             3 weeks ago
     maint/1.x       release 1.4.2                2 months ago
  git checkout  <branch>  [paths…]
```

Inspired by [fig](https://github.com/withfig/autocomplete) and [navi](https://github.com/denisidoro/navi). Written in Rust.

> [!WARNING]
> ezlo is at `0.x.x`: the CLI, the `.ezlo` language, the config format and the shell integration are all **unstable and may change in any release**, without a migration path. Pin a version if you depend on it.

> [!NOTE]
> **Built with AI.** More than 95% of ezlo's code was written by AI coding agents, with a human steering, reviewing and testing. If you'd rather use an AI-free tool, check out [navi](https://github.com/denisidoro/navi), an interactive cheatsheet tool for the command line.

---

## Install

```sh
# From crates.io (needs a Rust toolchain)
cargo install ezlo --locked
```

Alternatively, grab a [prebuilt binary](https://github.com/denisidoro/ezlo/releases) and put `ezlo` on your `$PATH`.

Then hook it into Zsh by adding this to your `~/.zshrc`, and open a new shell:

```sh
eval "$(ezlo init zsh)"
```

ezlo looks best with a [Nerd Font](https://www.nerdfonts.com/). See [Shell Integration & Keys](docs/shell.md) for keybindings and [Configuration](docs/configuration.md) for tuning.

---

## Highlights

### Completion that feels native
- **Inline dropdown** below the prompt, with aligned columns, Nerd Font icons, file-type colors, and rich rows (branch subjects, container states, file sizes, …). It never scrolls your terminal or pollutes scrollback.
- **Ghost text** from history (fish-style), accepted with `→`.
- **Signature hint** (`scp  <source…>  [target]`) that underlines the argument you're currently filling.
- **Preview pane** (`Alt-P`) for the selected row: a branch's log, a container's logs, a file's first lines.
- **Settings popup** (`Alt-K`): tune the current shell on the fly (suggestions on/off, rows, history rows, ghost text, hints, previews) without editing the config.
- **Smart contexts without any spec**: `VAR=value` / `export` assignments, `$VAR` names in any word, nested command lines (`sudo timeout 5 git …`), and pipelines (`cat data.json | jq .us` offers `.users`, `.users[].name`, …).

### Confidence before you press Enter
- **Live diagnostics**: unknown flags, conflicting options, missing required arguments and nonexistent paths are underlined as you type.
- **Effect preview**: `rm *.log` shows `⇢ rm: 12 files · 3.4 MB` before you run it; specs can declare safe dry-runs (`terraform plan`, `--dry-run`) that render live.
- **Instant values**: cheap commands are answered in-process without running anything: `echo $HOME` shows `= /home/you`, `cd src` shows where you'd land, `mkdir -p a/b` what it would create.
- **Comment → command**: type `# list files by size` and `Tab` suggests real commands through a translator you plug in (an LLM script; ezlo has no AI of its own).
- **Explain** (`Alt-H`): a token-by-token breakdown of the line: what each flag and argument means, which aliases were expanded, and where the suggestions come from.

### It learns from you
- **Usage memory**: what you accept and run is ranked by frequency, recency and project (git repo), and even becomes candidates for commands ezlo has no spec for.
- **Negative signals**: commands that keep failing sink in the ranking.
- **Empty-prompt recall**: `Tab` on an empty prompt lists what you usually run in this directory.
- **History search** (`Ctrl-R`): an inline picker over everything you ran, with directory, age and failures, scoped to the whole history, this repo or this directory.

### Recipes, not cheat sheets
- **`ezlo pick` (`Ctrl-G`)**: a fuzzy picker for ready-made command templates (`git fetch <remote> && git rebase <remote>/<branch>`) that fills each `<placeholder>` from live data, in dependency order, with previews.

### One small language
- **`.ezlo` files** describe types (live value sources), command grammars and recipes. They can be layered: your `git.ezlo` only needs to add what's missing from the bundled one.
- **Tooling**: `ezlo check` validates specs, `ezlo import` drafts one from `--help` (sandboxed), and `ezlo check --drift` reports flags your spec is missing or that the tool removed.

---

## A taste of `.ezlo`

```text
type container
  $ docker ps -a --format '{{.Names}}\t{{.State}}\t{{.Image}}'
  cols name state image
  preview `docker logs --tail 20 "$name" 2>&1`
  cache 2s

type container:running = container where state = running

cmd docker logs "Fetch the logs of a container"
  opt -f --follow "Follow log output"
  opt -n --tail <lines> = 10 | 100 | all "Number of lines to show"
  arg <container> container rank state = running

recipe "Follow the logs of a running container"
  $ docker logs -f <container>
  arg <container> container:running
```

That's enough for the dropdown to list containers (running ones first, with their state and image), preview their logs, flag `docker logs --nope` as an unknown option, and offer the recipe in `ezlo pick`.

---

## Documentation

| Guide | What's inside |
|---|---|
| [Completion](docs/completion.md) | The dropdown, ghost text, history rows, signature hint, preview pane, and spec-less contexts (assignments, nested commands, pipelines) |
| [Line Insight](docs/line-insight.md) | Live diagnostics, `Alt-H` explain, and effect previews (blast radius, instant values & dry-runs) |
| [Learning](docs/learning.md) | Usage memory, repo-scoped ranking, failure penalties, and empty-prompt recall |
| [Recipes & `ezlo pick`](docs/recipes.md) | Writing recipes and using the inline picker |
| [Writing Specs](docs/specs.md) | The `.ezlo` language: types, commands, options, arguments, and recipes |
| [Spec Tooling](docs/spec-tooling.md) | Spec locations and layering, `ezlo check`, `ezlo import`, and drift detection |
| [Shell Integration & Keys](docs/shell.md) | Zsh setup, keybindings, and `zstyle` styling |
| [Configuration](docs/configuration.md) | Every `config.toml` option |
| [CLI Reference](docs/cli.md) | All `ezlo` subcommands |
| [Editor Support](docs/editor.md) | Building, installing and using the VS Code syntax extension for `.ezlo` files |
| [Architecture](docs/architecture.md) | How the pieces fit: daemon, engine, store, and the wire protocol |

### Writing specs with an AI agent

[`skills/spec/`](skills/spec/SKILL.md) is an [agent skill](https://agentskills.io) that teaches coding agents the `.ezlo` language and how to port completions from zsh, fish, Fig, navi, man pages, `--help` output and tldr pages. Point your agent at it (or copy it into its skills directory) before asking for a new spec.

---

## Why "ezlo"?

In *The Legend of Zelda: The Minish Cap*, Ezlo is a grumpy Minish sage who gets turned into a green, beaked cap. Link wears him for the whole adventure, and Ezlo spends it sitting on his head, giving advice nobody asked for and occasionally saving the day. That's the job description here: sit on top of your shell, mutter useful hints, and help you shrink big commands into small ones. (Unlike the original, this one tries hard not to be grumpy.)
