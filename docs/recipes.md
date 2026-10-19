# Recipes & `ezlo pick`

A **recipe** is a ready-made command template with `<placeholders>`. `ezlo pick` lets you fuzzy-find a recipe, then fills each placeholder from live data, one at a time, and puts the finished command on your prompt.

---

## Using `ezlo pick`

Press `Ctrl-G` anywhere (or run `ezlo pick`):

```text
$ git █                                   ← press Ctrl-G
  ▸  Rebase onto the latest upstream     git fetch <remote> && git rebase <remote>/<branch>
     Fixup a commit and autosquash it    git commit --fixup <commit> && git rebase -i --autosquash <commit>~1
     Unstage files                       git restore --staged <files>
```

- **Context-aware**: the picker is seeded with what's already on the line, so `git` + `Ctrl-G` shows git recipes first, and the partial word becomes the search query.
- **Placeholders are filled in dependency order**: if `<branch>` depends on `<remote>`, you pick the remote first. Each step shows live candidates (branches, containers, files…) with the same rows as the dropdown, plus a preview pane when the type has a `preview` command.
- **Multi-select**: repeatable placeholders (`<files...>`) let you toggle several items with `Space`.
- **Free text**: if nothing matches, `Enter` uses what you typed.
- The result is inserted into your prompt for review; nothing runs unless you pass `--run`.
- In the dropdown, recipe rows are marked as recipes; accepting one (`Up`/`Down`, then `Enter`) opens the picker on it.

The picker renders inline below the prompt (about 40% of the terminal height, or `[pick] rows`), never switching to the alternate screen. Candidate lists load once per session and keystrokes only re-filter them; previews run asynchronously so typing never blocks.

- **Labels** take at most 35% of the terminal width, so descriptions and the preview keep their room.
- **Colors**: templates are colored like a shell line (command bold blue, flags cyan, operators and redirections magenta, quoted strings green), placeholders still to fill are magenta, and matched characters in recipe descriptions are yellow.
- **Preview**: a preview longer than its pane scrolls (keys below); its title then shows the position (`1-12/80 ⇅ Shift-↑↓`). A new preview starts at its top.

### Keys inside the picker

| Key | Action |
|---|---|
| `Up` / `Down`, `Ctrl-P` / `Ctrl-N`, `Shift-Tab` / `Tab` | Move the selection (wraps around) |
| `PageUp` / `PageDown` | Move one page |
| `Shift-Up` / `Shift-Down`, `Alt-Up` / `Alt-Down` | Scroll the preview by 3 lines |
| `Shift-PageUp` / `Shift-PageDown` | Scroll the preview by half a page |
| `Backspace`, `Ctrl-W` / `Alt-Backspace`, `Ctrl-U` | Delete a character / word / the whole query |
| `Space` | Toggle an item (repeatable placeholders) |
| `Ctrl-R` | Re-run the candidate generators |
| `Enter` | Pick the selection (or the typed text if nothing matches) |
| `Esc` | Go back to the previous placeholder or the recipe list, or quit |

---

## Writing recipes

Recipes live in `.ezlo` files, next to the command grammars:

```text
cmd git
  recipe "Rebase onto the latest upstream"
    $ git fetch <remote> && git rebase <remote>/<branch>
    arg <remote> git_remote
    arg <branch> git_branch

cmd git restore "Restore working tree files"
  opt -S --staged "Restore the index (unstage)"
  arg [pathspec...] git_changed_file
    when --staged: git_staged_file

  recipe "Unstage files"
    $ git restore --staged <files>

recipe "Display a notification"
  $ osascript -e 'display notification "<message>" with title "<title>"'
```

- **The template**: `$ <command>` is the command, with `<name>` placeholders. A placeholder can appear several times; it's asked for once.
- **Placeholder types**: an `arg <name> <type>` child line gives a placeholder its candidates, either a type (`git_branch`, `file`, `pid`, …) or fixed values (`arg <show> = true | false`).
- **Type inference**: without an `arg` line, ezlo infers the type from the command grammar. In `git restore --staged <files>`, `<files>` sits in `git restore`'s `[pathspec...]` slot, and since `--staged` is present, its `when --staged` clause picks `git_staged_file`.
- **Free text**: placeholders with no type or inferred type are typed freely.
- **Repeatable**: `<name...>` (or a variadic grammar slot) allows picking several values.
- **Scope**: a recipe nested under a `cmd` belongs to that command (its template must start with the command path, `E08` otherwise) and is offered when you're typing that command. A top-level recipe is global; templates that start with something else (`sudo …`, `until …`, `a | b`) must be top-level.
- **Dependencies**: a type parameterized by another placeholder (e.g. a type with `(remote = arg.remote)`) makes that placeholder be asked first. Cycles are reported by `ezlo check` (`E09`).

See [Writing Specs](specs.md) for types, parameters and `when` clauses.
