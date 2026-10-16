# Line Insight

ezlo doesn't just complete the line; it reads it. While you type, it checks the line against the command's spec, can explain it word by word, and previews what it would do.

---

## Live diagnostics

Problems are underlined directly in your command line (errors red, warnings yellow) and the first message is shown in the dropdown footer:

```text
$ git commit --amned -m "fix"
             ‾‾‾‾‾‾‾                  ← underlined in red
  ✗ unknown option '--amned'
```

What gets flagged, for commands that have a spec:

- **Unknown options**: `git commit --amned`.
- **Conflicting options**: two options the spec marks as mutually exclusive (`excludes`).
- **Repeated options**: an option given twice when it isn't repeatable.
- **Missing pieces**: a `required` option or a required positional argument, once the cursor has moved past where it should be; an option missing some of its values (`jq --arg name` with nothing after it, once the cursor is no longer at the end of the line).
- **Missing paths**: a `file`/`dir` argument that doesn't exist (a warning, since you might be about to create it).

Diagnostics follow nested command lines too: in `sudo timeout 5 git commit --amned`, each part is checked against its own command's grammar.

---

## Explain (`Alt-H`)

Press `Alt-H` (or run `ezlo explain --line '…'`) to dissect the command under the cursor. The explanation replaces the dropdown until your next edit or `Esc`:

```text
$ git checkout -b feature/login origin/main█        ← Alt-H
git checkout — Switch branches or restore working tree files  [bundled spec]
  git            command    Distributed version control system
  checkout       subcommand Switch branches or restore working tree files
  -b             option     -b <new_branch>: Create and checkout a new branch
  feature/login  value      value of -b <new_branch>
                 ↳ untyped: learned values from history
  origin/main    argument   [target]
                 ↳ type git_ref
▸ completing argument [target] from `origin/main`: 3 from bundled spec
```

It shows:

- **Expansion**: shell aliases, leading `VAR=value` assignments and wrappers (`sudo`, `env`, …).
- **The matched command**: its description and which spec layer it came from (e.g. `user spec over bundled spec` when your spec extends a bundled one).
- **Every word's role**: subcommand, option, option value (and which option it belongs to), positional argument (and which slot it fills), or unknown.
- **Value sources**: the type behind each value slot, how its parameters are resolved (`host ← opt.host`), and which `when` conditions are active.
- **Diagnostics** and the **effect preview** for the line.
- **Provenance**: how many of the current suggestions come from each source (`user spec`, `bundled spec`, `generated spec`, `learned history`, `native zsh`, `built-in`).

`ezlo explain --json` emits the same information as JSON.

---

## Effect preview

ezlo shows what a command would do before you run it, in extra lines below the prompt. They stay visible even while the dropdown is open: up to 2 lines (`zstyle ':ezlo:preview' max-lines`) are drawn under the status line, and warnings (`⚠`) go to the status line itself.

There are three kinds:

- `⇢` **effects**: what a command would change (blast radius, `cd` target, created directories).
- `=` **values**: what a read-only command would print (`echo $HOME`, `pwd`, `which ls`).
- **dry-run output** from a spec's `preview` command.

### Blast radius

For file-mutating commands (`rm`, `rmdir`, `mv`, `cp`, `chmod`, `chown`, `chgrp` by default), ezlo expands the arguments, globs included, and summarizes what they hit:

```text
$ rm -rf build/ *.log
  ⇢ rm: 12 files, 1 dir (+340 inside) · 48.2 MB

$ mv *.png assets/
  ⇢ mv: 2 files · 300 B → assets/

$ rm *.lgo
  ⚠ rm: no matches found: *.lgo
```

- Globs are expanded the way zsh does by default (`*`, `?`, `[…]`, `**/`, no hidden files unless the pattern starts with `.`). Quoted globs stay literal.
- Anything that needs the shell to evaluate (`$VAR`, `$(…)`, `{a,b}`) is skipped rather than guessed.
- A single plain file is summarized too (`rm a.log` → `rm: 1 file · 100 B`).
- Directories are measured recursively. Scans are capped in size and time, and capped numbers are shown as lower bounds (`≥`).
- It works behind wrappers and nested commands (`sudo rm …`, `timeout 5 rm …`).

### Safety warnings

ezlo warns (yellow `⚠`) about lines that are easy to regret, before you press Enter. Nothing runs to find out:

- `rm -rf ~/*`, `rm -rf /`, `chmod -R 777 /usr`: a destructive command aimed at `/`, your home or a system directory (also `rm -rf *` while in `~`).
- `rm -rf $BUILD_DIR/`: `$BUILD_DIR` isn't exported (or is empty), so the line may mean `rm -rf /`. `${BUILD_DIR:-build}` doesn't warn.
- `echo x > notes.txt`, `cp a.txt b.txt`: an existing file would be overwritten (`>>`, `cp -n`/`-i` don't warn).
- `git push --force` (use `--force-with-lease`), `dd of=/dev/sda`, `mkfs.ext4`, `chmod 777`.

Turn them off with `[preview] safety = false`.

### Instant previews

Many everyday commands are cheap enough to answer in-process, without running anything:

```text
$ echo $HOME/notes
  = /home/you/notes

$ cd ~/dotfiles
  ⇢ cd → ~/dotfiles · 26 entries

$ mkdir -p build/out
  ⇢ mkdir: creates build/out

$ ln -s nope link
  ⇢ ln: symlink link → nope
  ⚠ ln: nope doesn't exist (dangling link)
```

What's covered:

- **Values** of read-only commands, as if they had run:
  - `echo`/`print`, only when a word has something to expand (`$VAR`, `${VAR}`, `$((1+2))`, a leading `~` or an unquoted glob); `echo *.nope` warns `no matches found`.
  - `pwd` (`-P`), `whoami`, `hostname`, `nproc`, `printenv NAME`.
  - `basename`/`dirname`, `realpath`/`readlink`.
  - `which`/`type`/`whence` (aliases, then `$PATH`).
  - `test`/`[ … ]`: `true (exit 0)` or `false (exit 1)`.
- **Effects**:
  - `cd`/`pushd`: where you'd land and how many entries it has (`CDPATH` included); a warning when the directory doesn't exist.
  - `mkdir`: what it creates, what already exists, and missing parents without `-p`.
  - `touch`: which files it creates and which only get a new time.
  - `ln`: `symlink link → target`, with warnings for a dangling link or an existing one.
  - `kill`: the names of the processes it would signal.
- **Sizes** of what a reader would dump: `cat`/`wc` (lines, words, size; files are read up to 4 MiB), `ls` (dirs, files, hidden ones), `file` (a quick type guess).

Anything that needs the shell to run code is never evaluated: `$(…)`, backticks, `$1`, `$?` and `${x:-y}` give no preview, and neither does a command with redirections. Pick the commands with `[preview] instant` (`[]` turns them off; see [Configuration](configuration.md)).

### Dry-runs from specs

A spec can declare a side-effect-free command that previews what the real one would do. ezlo runs it as you edit, with the values you've typed filled in:

```text
cmd terraform apply "Apply changes"
  opt -target <resource> "Resource to target"
  arg [plan]
  preview $ terraform plan -no-color {opt.target} {arg.plan} 2>&1 | tail -n 3
```

```text
$ terraform apply -target aws_s3_bucket.logs
  ⇢ Plan: 1 to add, 0 to change, 0 to destroy.
```

- `{opt.<name>}` and `{arg.<name>}` are replaced by the parsed, shell-quoted values; absent ones become empty. Nothing runs until every referenced required argument is typed.
- Output is cached briefly, limited to 3 lines, and the command is killed after a timeout (`[preview] timeout`, default 500 ms).

> [!WARNING]
> Dry-run commands run while you type. Only use commands that are genuinely side-effect free (`plan`, `diff`, `--dry-run`, `-n`).

Configure or disable these previews in the `[preview]` table (see [Configuration](configuration.md)). `ezlo preview --line '…'` prints the preview from the CLI.
