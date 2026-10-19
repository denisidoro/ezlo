---
name: ezlo-spec
description: >-
  Writes, converts and validates `.ezlo` files for `ezlo` (a spec-driven Zsh autocomplete and recipe launcher). Use this whenever creating or editing a `.ezlo` file (user specs in `~/.config/ezlo/specs/`, bundled ones in the ezlo repo's `specs/`), adding completions or recipes for a CLI tool, or porting completions/snippets from zsh `_arguments` files, fish `complete` files, navi `.cheat` files, man pages, `--help` output, Fig specs or tldr pages.
---

## Before writing anything

- Check whether the command is already covered: `rg -l '^cmd <tool>\b' ~/.config/ezlo/specs <ezlo-repo>/specs`. Extend the existing file instead of creating a second one (a `cmd` path declared twice in the same tier triggers `E04` and only the first file wins).
- Pick the source to port from (see [Sources](#sources)) and read its reference file. Prefer structured sources (Fig > fish > zsh `_arguments` > man > `--help`) for the grammar, and navi/tldr for recipes.
- Never hand-edit generated specs (files whose header says another tool produces them): fix the generator and rerun it.

## Where files go

| Location | Use for |
| --- | --- |
| `~/.config/ezlo/specs/<tool>.ezlo` | Default for personal specs (or any dir in `[sources.user] dirs` / `$EZLO_SPECS_DIR`; hot-reloaded). Group small related tools in one themed file (`network.ezlo`, `process.ezlo`) |
| `<ezlo-repo>/specs/<tool>.ezlo` | Bundled into the binary; only for broadly useful tools (see `src/bundled.rs` for the current list). Also register it there and rebuild |
| `~/.cache/ezlo/generated/` | Output of `ezlo import <cmd>`; a starting point, never the final home |

Start each file with a `#` comment line saying what it covers (and any platform caveats, e.g. "BSD/procps-compatible `ps` flags").

## Syntax cheat sheet

Indentation-based (spaces only, no tabs), one node per line, `#` comments (full-line, or trailing after whitespace on non-`$` lines; `#` inside `"..."` and in `$` shell lines is literal). Strings use `"..."` with escapes `\" \\ \n \t` only. Reserved words can't be names: `type cmd opt arg recipe when where rank and in global required excludes cols insert show preview cache values effect`. Top-level blocks: `type`, `cmd`, `recipe`, `alias`.

### `type`: candidate sources

```
type docker_container(host = opt.host | env.DOCKER_HOST | "unix:///var/run/docker.sock")
  $ docker ${host:+-H "$host"} ps -a --format '{{.ID}}\t{{.Names}}\t{{.State}}' 2>/dev/null
  cols id name state            # TSV columns (default: one column named `value`)
  insert name                   # column inserted on the line (default: first col)
  show name state               # columns displayed (default: all cols)
  preview `docker ${host:+-H "$host"} logs --tail 20 "$id" 2>&1`
  cache 2s                      # ms, s, m, h

type restart_policy
  values no always unless-stopped on-failure   # static alternative to `$`; exactly one of `$`/`values`

type docker_container:running = docker_container where state = running
type docker_container:stopped = docker_container where state in (created, exited, dead)
type bazel_target:test = bazel_target where kind ~ "_test$" and name != foo
```

- `$` lines are raw shell (`\` at end of line continues). Output must be TSV, one row per candidate.
- Params become shell vars in `$`/`preview` (`$host`); columns become vars in `preview` (`$id`). Sources: `opt.<name>` (canonical name = first `--long` without dashes, else the first flag without its dash: `-s` → `s`), `arg.<name>` (positional/recipe placeholder), `env.<VAR>`, `"literal"`, chained with `|` (first non-empty wins; nothing may follow a literal, `W01`).
- Refinement names should be `<base>:<variant>` (`W03` otherwise); refinements have no children.
- Builtin types: `file`, `dir`, `path` (rich path rows, no declaration needed).
- Use-site expressions: `type_name(param = source) where <clauses> rank <clauses>`; `where` drops non-matching rows, `rank` keeps them but de-emphasizes them.

### `cmd`: grammar

```
cmd docker "Container runtime"
  opt -H --host <host> "Daemon socket to connect to" global
  opt -c --context <context> docker_context "Context to use" global
  opt --log-level <level> = debug | info | warn "Logging level"
  opt -v --verbose "Verbose output"
  opt -q --quiet "Only show IDs" excludes -v --verbose
  opt --token <token> "API token" required
  opt -name <pattern> "Single-dash long flag (find -name, nmap -sS)"

cmd docker run "Create and run a new container"
  opt -e --env <env...> "Set environment variables"      # repeatable value
  opt --network <network> docker_network "Connect to a network"
  arg <image> docker_image                                # required positional
  arg [command...]                                        # optional, variadic (last only)

cmd brew install "Install a formula or cask"
  opt --cask "Treat all arguments as casks"
  opt --format <format> = json | text "Output format"
  arg <formula...> brew_formula
    when --cask: brew_cask                                # conditional type
    when !--cask and --format=json: brew_formula:json     # negation, value match, `and`
```

- Line order: `opt <flags...> [<value>] [type-expr] [= sugg | "sugg"] ["desc"] [global] [required] [excludes <flags...>] [effect <class>] [dryrun]`, and `arg <name>|[name] [type-expr] [= suggestions] ["desc"] [effect <class>]`.
- Side effects: an `effect read|write|remote|destroy` child line declares what the command changes (subcommands without one inherit it, so declare it per subcommand when the parent's is wrong for them); `effect <class>` on an `opt`/`arg` raises it when used (`opt -D "…" effect destroy`, `arg [name] effect write`); `dryrun` on an `opt` makes the line `read` (`-n`, `--dry-run`, `--list`). Leave commands that run arbitrary code (`foreach`, `exec`, `-c <script>`) without one: undeclared means unknown, never safe.
- `cmd a b c` declares a subcommand path; parent `cmd a b` lines may be bare. `global` opts are inherited by every subcommand.
- `when` and `excludes` flags must be declared in the same `cmd` (or be `global` on an ancestor), else `E05`. `when` on a valueless opt is `W02`.
- Placeholder names are lowercase snake_case (`<image_id>`, not `<image-id>`/`<IMAGE>`); anything else in `<...>` stays literal text.

### `alias`: command words standing for other words

```
alias gco = git checkout          # `gco ` completes/validates/explains as `git checkout `
alias j = __jump                  # shell functions and wrapper scripts ezlo can't see as aliases
alias gfmt = git log "--format=%h %s"   # quote words with spec punctuation
```

- Top-level only; one level of expansion. Beats a shell alias of the same name; higher spec layer wins.
- Not offered as a candidate (the shell must know the name). `E04` for a duplicate name or for hiding a `cmd` root of the same file.
- Different from `alias co` inside a `cmd` block, which adds an alternative spelling of that subcommand.

### `recipe`: ready-made commands for `ezlo pick`

```
cmd docker run "..."
  recipe "Run a throwaway shell in an image"
    $ docker run --rm -it --entrypoint <shell> <image>
    arg <shell> = sh | bash
                                     # <image> needs no `arg`: inferred from `cmd docker run`'s grammar

recipe "Serve the current directory over HTTP"   # top-level: any command
  $ python3 -m http.server <port>
  arg <port> = 8000 | 8080
```

- Exactly one `$` template (multi-line via `\`), plus optional `arg` lines; only `$` and `arg` are allowed inside.
- A recipe nested under `cmd x y` must start with `x y` (`E08`); otherwise make it top-level.
- Placeholder types resolve in this order: explicit `arg` line, the grammar slot it sits in, a type with the same name. Only add `arg` lines when inference doesn't give the right type.
- Variadic: template `<items>` + `arg <items...> type` (multi-select with `Space`). Escape a literal `<x>` as `\<x>`.
- Dependencies: a type param `arg.<ph>` makes that placeholder get filled first (cycles are `E09`).

## Conventions

- Names: `<tool>_<noun>` snake_case types (`adb_package`, `tmux_session`), refinements `<type>:<state>`.
- Descriptions: short, sentence case, no trailing period, imperative or noun phrase matching the tool's own help.
- Generators: fast (default kill timeout `1s`), quiet (`2>/dev/null`), deterministic TSV via `--format`/`awk`/`jq -r ... | @tsv`, `tr -d '\r'` for adb/Windows output, a `cache` on anything slower than ~50 ms. Must work on both macOS and Linux unless the file header says otherwise.
- Honor connection flags: model `-H/--host`, `-s/--serial`, `--context` as params with `opt.x | env.Y` defaults and pass them via `${x:+-H "$x"}`.
- `insert` the identifier, `show` the human-readable columns, `preview` for anything that helps choose (logs, `git show`, `inspect`).
- Types are file-local: `ezlo check` validates each file in isolation, so a type used in a file must be declared in that file (duplicate small types rather than referencing another file's).
- Prefer grammar over recipes: if a navi/tldr snippet is just "command + one typed arg", the `cmd` grammar already covers it; keep recipes for multi-step pipelines or non-obvious flag combos.
- Don't model every flag of huge tools; cover the flags people actually type, plus anything that takes a value worth completing.

## Validation

Validate on a host where ezlo and the tool are installed (projects may say where that is):

```bash
chmod 644 <dir>/<tool>.ezlo && ezlo check <dir>/<tool>.ezlo
# smoke test completions and generators against the real tool
ezlo complete --line "<tool> <sub> "
ezlo complete --json --line "<tool> --flag "
```

- `ezlo check` must print `ok` with no warnings. Codes: `E01` lexical, `E02` structure, `E03` type form, `E04` duplicate, `E05` unresolved, `E06` reserved word, `E07` positional order, `E08` recipe structure, `E09` recipe cycle, `E10` env/col names, `W01`–`W03` warnings.
- ezlo silently skips spec files that are group/world-writable or owned by another user (it runs their shell generators). With a `002` umask new files land as `0664`: always `chmod 644`, and run `ezlo check` to see `skipping ...` warnings.
- For a scratch spec, `EZLO_SPECS_DIR=/tmp/<dir> ezlo complete --line '...'` tests it without touching your real specs.
- Run each `$` generator by hand once (with params exported) to confirm the TSV shape matches `cols`.
- If a tool isn't installed where you validate, say the generator is unverified instead of claiming it works.

## Sources

| Source | Best for | Reference |
| --- | --- | --- |
| zsh completion (`_arguments`, `_describe`) | Grammar, exclusions, value types | [zsh-completion.md](references/zsh-completion.md) |
| fish completion (`complete -c`) | Grammar with descriptions, subcommand conditions | [fish-completion.md](references/fish-completion.md) |
| Fig spec (`withfig/autocomplete`) | Grammar, generators, suggestions (closest model to ezlo) | [fig-spec.md](references/fig-spec.md) |
| navi `.cheat` | Recipes and variable generators | [navi-cheat.md](references/navi-cheat.md) |
| man page | Authoritative flag list and positional synopsis | [man-page.md](references/man-page.md) |
| `--help` output, `ezlo import`, cobra/clap | Quick first draft, dynamic completion callbacks | [help-output.md](references/help-output.md) |
| tldr page | A handful of high-value recipes | [tldr-page.md](references/tldr-page.md) |
