# Writing Specs

A `.ezlo` file teaches ezlo about commands. It has four kinds of blocks:

- **`type`**: a named source of values (branches, containers, profiles…).
- **`cmd`**: the grammar of a command or subcommand: its options and positional arguments.
- **`recipe`**: a command template with placeholders, for [`ezlo pick`](recipes.md).
- **`alias`**: a command word that stands for other words (`alias gco = git checkout`), see [Command aliases](#command-aliases-alias).

Blocks are indentation-based (two spaces, no tabs). `#` starts a comment.

```text
type git_branch
  $ git for-each-ref --format='%(refname:short)%09%(contents:subject)%09%(committerdate:relative)' refs/heads
  cols branch subject age
  insert branch
  show branch subject age
  preview `git log -n 20 --oneline --color=always "$branch"`
  cache 2s

cmd git checkout "Switch branches or restore files"
  alias co
  opt -b <new-branch> "Create and switch to a new branch"
  opt -f --force "Throw away local modifications"
  arg [branch] git_branch
  arg [paths...] file
```

A fuller example, with a filtered type and a recipe:

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

Run `ezlo check my.ezlo` after editing; it reports problems with line and column (see [Spec Tooling](spec-tooling.md)).

---

## Types

A type produces **rows**. Each row has one or more columns.

### Generated values

```text
type container(host = opt.host | env.DOCKER_HOST)
  $ docker ${host:+-H "$host"} ps -a --format '{{.ID}}\t{{.Names}}\t{{.State}}\t{{.Image}}'
  cols id name state image
  insert name
  show name state image
  preview `docker ${host:+-H "$host"} logs --tail 20 "$id" 2>&1`
  cache 2s
```

| Line | Meaning |
|---|---|
| `$ <shell>` | Command whose stdout lines are rows, columns separated by tabs. Runs with `sh -c` in your shell's directory and environment, killed after `[generators] timeout` (default 1 s). |
| `cols a b c` | Column names, in order. Without `cols`, each line is a single `value` column. |
| `insert <col>` | Column inserted into the command line (default: the first). |
| `show a b` | Columns shown in the dropdown as aligned data columns (default: all). |
| `preview` `` `<shell>` `` | Command shown in the [preview pane](completion.md#preview-pane-alt-p) and in `ezlo pick`. Columns are available as `$<col>`. |
| `cache <duration>` | How long rows stay fresh (`500ms`, `2s`, `1m`). Stale rows are shown while they refresh in the background. |

### Static values

```text
type log_level
  values off error warn info debug trace
```

`values` takes bare words or quoted strings. A type has either `$` or `values`, not both.

### Parameters

A type can take parameters, filled from the line being completed:

```text
type defaults_key(domain = arg.domain)
  $ defaults read "$domain" | awk -F' = ' '/^    [A-Za-z]/ { gsub(/ /, "", $1); print $1 }'
```

- Parameters are environment variables of the `$` and `preview` commands (`$domain`, `${host:+…}`).
- Each parameter is a fallback chain: `opt.<name>` (an option's value), `arg.<name>` (a positional argument), `env.<NAME>` (an environment variable), or a `"literal"`, separated by `|`. The first source with a value wins.
- A caller can also bind parameters explicitly: `arg <key> defaults_key(domain = "NSGlobalDomain")`.

### Refinements

A refinement is a filtered view of another type. Its name must start with the base type's name (`container:…`):

```text
type container:running = container where state = running
type container:stopped = container where state in (created, exited, dead)
type session:detached = session where state != attached
```

---

## Commands

```text
cmd docker "Docker CLI"
  opt -H --host <host> "Daemon socket to connect to" global

cmd docker logs "Fetch the logs of a container"
  opt -f --follow "Follow log output"
  opt -n --tail <lines> = 10 | 100 | all "Number of lines to show"
  arg <container> container rank state = running
```

- **Subcommands** are separate top-level blocks with the full path: `cmd docker logs`, `cmd git remote add`. (Nesting `cmd` blocks is not allowed.)
- **Descriptions** are the optional quoted string after the path.
- **Aliases**: `cmd git (checkout|co) "…"` (first word is canonical) or an `alias co sw` line. Aliases are walked, validated and learned as their canonical word. They're offered only when the typed word is a prefix of the alias, so `git <Tab>` isn't cluttered.

### Command aliases (`alias`)

A top-level `alias` line makes a command word behave like other words, the same way a shell alias does:

```text
alias gco = git checkout
alias gcb = git checkout -b
alias j = __jump
alias gfmt = git log "--format=%h %s"
```

- Typing `gco ` completes, validates and explains the line as `git checkout ` (expansion happens once the cursor has left the alias word, one level deep, like zsh).
- Use it for names ezlo can't see as shell aliases: shell functions, wrapper scripts, or a shell alias you want completed with a different grammar.
- Precedence: a spec alias wins over a shell alias of the same name (so `alias gco = git checkout` overrides a shell `gco='dot git checkout'`); across layers the higher one wins (user > bundled > generated).
- Expansion words are bare or `"quoted"`; quote any word holding spec punctuation (`= : | ( ) , ~ < > [ ]`).
- Unlike `alias` inside a `cmd` block (alternative spellings of a subcommand), a top-level alias isn't offered as a candidate: the shell must actually know the name.
- `ezlo check` rejects a duplicate alias name in a file, and an alias that hides a `cmd` root of the same file (unless it expands to that root, e.g. `alias ls = ls -la`).

### Options (`opt`)

```text
opt -s --short "Give the output in the short format"
opt -u --untracked-files <mode> = all | normal | no "Show untracked files"
opt --source <tree> git_ref "Restore from the given tree-ish"
opt --json "JSON output" excludes --yaml
opt --color [when] = auto | always | never "Colorize output"
opt --slurpfile <name> <file> file "Bind a variable to a file's JSON values"
```

The general form is `opt <flags…> [<placeholder> [values]]… ["description"] [modifiers…]`, in that order (modifiers come after the description).

- **Flags**: short `-x` (clusterable: `-la`, `-n5`), long `--name`, and single-dash long `-name` (`find -name`).
- **Value**: an option takes a value when it has a `<placeholder>`. The value's candidates come from:
  - a type: `<tree> git_ref`, optionally refined with `where …` / `rank …`;
  - fixed suggestions: `<mode> = all | normal | no`;
  - or the placeholder name, if it matches a [built-in type](#built-in-types) (`<file>`, `<dir>`);
  - or values you used before ([learned](learning.md)).
- **Optional value**: a `[placeholder]` (`opt --color [when] = auto | always | never`) makes the value optional, like GNU `--color[=WHEN]`. As with `getopt`, the value only counts when attached: `--color=always`, or `-iSUFFIX` for a short flag. In `--color always`, `always` is the next positional, not the value. Completion offers both `--color` and `--color=`, and values after `--color=`. Add `greedy` when the tool also takes a separate next word (`opt -b --boot [boot] = 0 | -1 | -2 "…" greedy` for `journalctl -b -1`). That word is taken as the value only if it's one of the fixed suggestions or an integer (`-5`); anything else stays a positional or option. After `-b `, the suggestions are offered next to the usual candidates.
- **Several values**: list one placeholder per value (`opt --arg <name> <value>`). Each placeholder can have its own type and suggestions, written right after it like an `arg`'s (`opt --slurpfile <name> <file> file`). The values fill the next words in order, and each position is completed separately. `--arg=n` fills the first value, and the rest follow as words. `explain` shows which placeholder each word fills, and `{opt.arg}` in a `preview` expands to all the values. A `[placeholder]` must be the option's only value, and only the last value can end in `...` (which makes the option repeatable). `when` clauses refine the first value only.
- **Modifiers**:
  - `global`: also valid on every subcommand.
  - `required`: flagged as missing once you've moved past it.
  - `hidden`: recognized and validated, but only suggested once you type a prefix of it (keeps rarely-used flags out of the list).
  - `excludes --a --b`: mutually exclusive with those flags.
  - `greedy`: only for an optional value (see above).
- **Repeatable options**: a `...` placeholder (`opt --exclude <pattern...>`) may be given several times without a duplicate warning.
- **Limits**: flag names can't contain `.` or be `-@` (e.g. curl's `--http1.1`).

### Positional arguments (`arg`)

```text
arg <source...> path
arg [target] host
arg [pathspec...] git_changed_file
  when --staged: git_staged_file
```

- `<name>` is required, `[name]` is optional, and `...` makes it repeatable (only the last argument may be repeatable).
- The value part works like options: a type, `= a | b`, a built-in by name, or learned values.

### Conditional types (`when`)

An option value or argument can change its type depending on other flags on the line:

```text
cmd brew uninstall
  opt --cask "Treat all named arguments as casks"
  arg <formula...> brew_installed_formula
    when --cask: brew_installed_cask
```

### Filtering and ranking (`where`, `rank`)

- `where <condition>` keeps only matching rows: `arg <c> container where state = running`.
- `rank <condition>` keeps every row but puts matching ones first: `arg <c> container rank state = running`.
- Conditions compare a column: `=`, `!=`, `in (a, b)`.

### Composite values (`list`, `kv`)

Some values are several parts in one word. Add a modifier right after the placeholder:

```text
opt --features <features> list(",") cargo_feature    # serde,derive
opt -o <option> kv("=") ssh_option                   # -o StrictHostKeyChecking=no
arg <destination> kv("@", ssh_host) user             # user@host
```

- `list("<sep>")`: completes only the item under the cursor and hides items already used.
- `kv("<sep>")` / `kv("<sep>", <type>)` (the separator can't contain whitespace; `kv(":")` works for `Name: value` headers): left of the separator, the slot's values are offered as keys; right of it, values for that key (and rows of the optional type) are offered, keeping the key in place.

### Nested command lines (`cmd` type)

An argument of type `cmd` means "the rest is another command":

```text
cmd timeout "Run a command with a time limit"
  opt -s --signal <sig> signal
  arg <duration>
  arg <command...> cmd
```

On the slot itself, ezlo offers executables and aliases. Once a command is typed there, the rest of the line is completed, validated and explained as that command (`timeout 5 git checkout --f` completes `git checkout` flags).

> [!NOTE]
> Words listed in `[line] wrappers` (`sudo`, `env`, `time`, `nice`, `nohup`, `command`, `exec`, `xargs` by default) are skipped before spec lookup, so a spec for one of them is never used: `xargs -<Tab>` completes commands, not `xargs` flags. Remove a word from `wrappers` if you want to describe it with a spec (and a `cmd`-typed argument) instead.

### Dry-run previews

A `cmd` can declare a side-effect-free command that ezlo runs while you edit, to show what the real one would do:

```text
cmd terraform apply
  opt -target <resource>
  arg [plan]
  preview $ terraform plan -no-color {opt.target} {arg.plan} 2>&1 | tail -n 3
```

See [Line Insight](line-insight.md#dry-runs-from-specs).

---

## Recipes

```text
cmd git
  recipe "Rebase onto the latest upstream"
    $ git fetch <remote> && git rebase <remote>/<branch>
    arg <remote> git_remote
    arg <branch> git_branch
```

See [Recipes & `ezlo pick`](recipes.md#writing-recipes) for the details.

---

## Built-in types

These need no declaration (a declared type with the same name replaces the built-in). Their extra columns can be used in `where`/`rank`.

| Type | Values | Columns |
|---|---|---|
| `file`, `dir`, `path` | Files and folders, with size/item count, age and file-type colors | `value` |
| `cmd` | Shell aliases, then executables in `$PATH` | `value kind location` |
| `env` | Environment variables, with their current values | `value current` |
| `pid` | Running processes, newest first | `value name user args` |
| `signal` | POSIX signals (`HUP`, `INT`, `TERM`, `KILL`, …) with numbers | `value number desc` |
| `port` | Listening TCP / bound UDP ports | `value proto address process` |
| `user`, `group` | System accounts and groups | `value uid name home shell` / `value gid members` |
| `host` | Hosts from `~/.ssh/config`, `~/.ssh/known_hosts` and `/etc/hosts` | `value source` |
| `interface` | Network interfaces | `value state address` |

- **Path types by name**: `file`, `dir` and `path` also apply automatically when a placeholder has that name (`arg <file>`).
- **System types are explicit**: reference them by type (`arg <pid...> pid`). A placeholder named `<port>` stays untyped, so `ssh -p <port>` doesn't list your local ports.
- **Refinable**: `type signal:fatal = signal where value in (KILL, TERM)`.
