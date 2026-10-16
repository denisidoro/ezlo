# Porting fish completion files

fish completions are declarative one-liners, so they port almost mechanically and usually carry good descriptions.

## Finding the source

- `/usr/share/fish/completions/<cmd>.fish`, `$(brew --prefix)/share/fish/vendor_completions.d/`, `~/.config/fish/completions/`.
- If fish isn't installed: fetch upstream from `fish-shell/fish-shell` (`share/completions/<cmd>.fish`) or generate via `<cmd> completion fish`.

## `complete` flags

| fish | ezlo |
| --- | --- |
| `-c cmd` | root command |
| `-s f` / `-l force` / `-o name` | `-f` / `--force` / `-name` (old-style single-dash long); combine all on one `opt` line |
| `-d 'desc'` | `"desc"` |
| `-r` (requires a value) | add a `<value>` placeholder (name it from the desc or `-a` content) |
| `-x` (= `-r -f`, value, no files) | `<value>` with the type/suggestions from `-a` |
| `-F` / `-r -F` | value type `file` |
| `-f` alone on a positional rule | positional isn't a file: use the `-a` list instead |
| `-a 'one two'` | `= one \| two` (or `values` type) |
| `-a 'one\tDesc two\tDesc'` | `type` with `values` loses the desc; prefer `$ printf 'one\tDesc\ntwo\tDesc\n'` + `cols value desc` |
| `-a '(cmd list)'` | `type` with `$ cmd list` (fish strips after `\t` for descriptions: keep the tab as the 2nd column) |
| `-a '(__fish_complete_directories)'` | `dir` |
| `-a '(__fish_complete_pids)'`, `__fish_complete_users`, `__fish_print_hostnames` | copy the matching type (process, user, host) into the file |
| `-k` (keep order) | drop (ezlo ranks by frecency) |
| `-w other` (wraps) | copy the wrapped command's grammar only if needed |

## Conditions (`-n`)

| fish condition | ezlo |
| --- | --- |
| `__fish_use_subcommand` + `-a 'sub'` | `cmd <tool> sub "desc"` |
| `__fish_seen_subcommand_from sub` | the rule belongs to `cmd <tool> sub` |
| `__fish_seen_subcommand_from a b` | duplicate the opt in each of `cmd <tool> a` / `cmd <tool> b` (or make it `global` on the parent if every subcommand has it) |
| `__fish_contains_opt cask` | `when --cask: <type>` on the affected arg/opt |
| `not __fish_contains_opt x` | `when !--x: <type>` |
| custom `__fish_<tool>_needs_command` / `using_command sub` helpers | read the helper: usually the same as the two subcommand rows above |
| arbitrary shell conditions | usually not representable; keep the most common case |

## Example

```fish
complete -c brew -n __fish_use_subcommand -a install -d 'Install a formula or cask'
complete -c brew -n '__fish_seen_subcommand_from install' -l cask -d 'Treat all arguments as casks'
complete -c brew -n '__fish_seen_subcommand_from install' -s s -l build-from-source -d 'Compile from source'
complete -c brew -n '__fish_seen_subcommand_from install; and not __fish_contains_opt cask' -xa '(brew formulae)'
complete -c brew -n '__fish_seen_subcommand_from install; and __fish_contains_opt cask' -xa '(brew casks)'
```

```
type brew_formula
  $ brew formulae 2>/dev/null
  cache 1h

type brew_cask
  $ brew casks 2>/dev/null
  cache 1h

cmd brew install "Install a formula or cask"
  opt --cask "Treat all arguments as casks"
  opt -s --build-from-source "Compile from source"
  arg <formula...> brew_formula
    when --cask: brew_cask
```
