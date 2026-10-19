# Spec Tooling

How ezlo finds and layers spec files, and the commands that help you write and maintain them.

---

## Where specs come from

ezlo combines three layers of specs, from highest to lowest priority:

| Layer | Location | Notes |
|---|---|---|
| **User** | Every `*.ezlo` (recursively) under `[sources.user] dirs`, else `$EZLO_SPECS_DIR` (colon-separated), else `~/.config/ezlo/specs/` | Reloaded automatically when you save |
| **Bundled** | Compiled into the binary | `ezlo` itself, `git` and common file tools (`cd`, `ls`, `cat`, `rm`, `chmod`, …) |
| **Generated** | `~/.cache/ezlo/generated/*.ezlo` | Created by `ezlo import` |

For commands that no spec knows, ezlo falls back to native zsh completion. `[sources] enabled` turns individual sources off.

> [!NOTE]
> User spec files must not be writable by other users (or by a shared group); ezlo skips them otherwise.

### Layers are merged, not replaced

A user spec only needs what it adds or changes. When several layers declare the same `cmd`:

- the higher layer's description wins, and its aliases are added;
- each of its options replaces the lower-layer option with a matching flag; other options are kept;
- each of its arguments replaces the lower-layer argument with the same name; new ones are appended;
- subcommands, types and recipes declared only by a lower layer are kept.
- top-level `alias` declarations of a higher layer win over a lower layer's with the same name.

For example, this user spec keeps all of the bundled `git` grammar while adding an alias, a hidden flag and a subcommand:

```text
cmd git checkout
  alias co
  opt --guess "Guess remote branch" hidden

cmd git wip "Commit everything as WIP"
```

Within one layer, files can extend each other (e.g. `specs/dot/_root.ezlo` + `specs/dot/deploy.ezlo`). If two files of the same layer declare the same `cmd` path, the first one (sorted by path) wins and `ezlo check` warns about the duplicate.

---

## `ezlo check`: validate specs

```sh
ezlo check                    # bundled, generated and user specs, plus config.toml
ezlo check path/to/tool.ezlo  # specific files
```

Type references in a checked file resolve against that file and your configured spec directories. A draft kept elsewhere that uses types declared in another file reports `E05`: move it into your spec directory (or check it together with a file declaring the types there).

Errors and warnings point at the exact line and column:

```text
error[E02]: unexpected token in 'opt' line; expected modifier ('global', 'required', 'hidden', 'greedy', 'excludes')
  --> tool.ezlo:20:30
   |
20 |   opt --json excludes --yaml "JSON output"
   |                              ^^^^^^^^^^^^^
```

| Code | Meaning |
|---|---|
| `E01` | Lexical error (tab indentation, bad indentation, unterminated string or backtick, unknown escape) |
| `E02` | Structure: a line that isn't allowed there, or a property given twice |
| `E03` | Type form: a type needs exactly one of `$` or `values`; refinements take no children |
| `E04` | Duplicate type name, command path, flag spelling or alias |
| `E05` | Unresolved reference: unknown type, column, parameter, flag, or `{opt.*}`/`{arg.*}` name |
| `E06` | Reserved word used as a name |
| `E07` | Positional argument order (required after optional, repeatable not last) |
| `E08` | Recipe structure (missing `$`, unknown placeholder, template outside its command's scope) |
| `E09` | Recipe placeholder dependency cycle |
| `E10` | Invalid or colliding column/parameter name |
| `W01` | Unreachable fallback after a literal in a parameter chain |
| `W02` | `when` on an option that takes no value |
| `W03` | Refinement name prefix differs from its base type |

`ezlo check` also reports unknown keys in `config.toml`.

`ezlo parse --json <file>` prints the parsed spec as JSON, which is handy for debugging or tooling.

---

## `ezlo import`: draft a spec from `--help`

```sh
ezlo import rg
# → ~/.cache/ezlo/generated/rg.ezlo
```

ezlo runs `<cmd> --help` inside a sandbox (Landlock + seccomp on Linux, `sandbox-exec` on macOS: no network, read-only filesystem, short timeout) and turns the options and subcommands it finds into a generated spec. Generated specs have the lowest priority, so you can copy one into your user specs as a starting point, or write a small user spec on top of it.

---

## `ezlo check --drift`: keep specs up to date

CLIs evolve. Drift detection compares your specs with what each tool's `--help` says today, using the same sandboxed probe as `ezlo import`:

```text
$ ezlo check --drift tool
tool
  + flag -q, --quiet              Quiet output
  + cmd  deploy                   Deploy things
  - flag --old                    declared in spec, not in --help
tool build
  + flag --release                Release mode
error: drift detected in 2 command(s)
```

- **`+ flag` / `+ cmd`**: the tool advertises options or subcommands your spec doesn't declare.
- **`- flag`**: your spec declares a flag the tool no longer lists. Hidden options are never reported.
- **`?`**: the path was skipped (the probe failed, the help had nothing recognizable, or a subcommand just repeated its parent's help).

Without arguments, every command declared in your user specs is checked (the root and up to 64 subcommand paths each). The exit code is non-zero when drift is found. Help parsing is heuristic, so treat the report as a review list.

---

## Writing specs with an AI agent

The repo ships an [agent skill](../skills/spec/SKILL.md) with the `.ezlo` syntax, conventions, validation steps and porting guides for zsh/fish completions, Fig specs, navi cheats, man pages, `--help` output and tldr pages. Load it in your coding agent, ask for a spec, then review it with `ezlo check` and `ezlo check --drift` like any other.
