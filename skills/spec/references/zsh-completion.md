# Porting zsh completion files

## Finding the source

- Installed functions: `zsh -c 'print -l $^fpath/_<cmd>(N)'`. Typical homes: `/usr/share/zsh/functions/Completion/*/` (Linux), `/usr/share/zsh/<ver>/functions/` (macOS), `$(brew --prefix)/share/zsh/site-functions/`, plugin dirs (`~/.cache/antidote/...`), `shell/zsh/completions/` here.
- Upstream: `zsh-users/zsh` (`Completion/`), `zsh-users/zsh-completions` (`src/_<cmd>`).
- Tool-generated: `<cmd> completion zsh`, `<cmd> --completions zsh`, `<cmd> generate-shell-completion zsh`. If the output only calls back into the binary (cobra `__complete`, clap `COMPLETE=zsh`), it has no static grammar: see [help-output.md](help-output.md#dynamic-completion-callbacks).

## `_arguments` mapping

| zsh | ezlo |
| --- | --- |
| `'-f[desc]'` | `opt -f "desc"` |
| `'(-f --force)'{-f,--force}'[desc]'` | `opt -f --force "desc"` (the `(...)` list naming its own aliases is just mutual exclusion with itself; drop it) |
| `'(-q --quiet)-v[desc]'` | `opt -v "desc" excludes -q --quiet` |
| `'-o+[desc]:file:_files'`, `'-o[desc]:file:_files'`, `'--out=[desc]:file:_files'` | `opt -o <file> file "desc"` (`+`, `=`, `=-` only change how the value attaches; ezlo accepts `-ofile`, `-o file`, `--out=file`) |
| `'*-v[desc]'`, `'*'{-e,--env}'[desc]:var:'` | repeatable: valueless opts need nothing; valued ones use `<var...>` |
| `'-name[desc]:pattern:'` (single-dash long) | `opt -name <pattern> "desc"` |
| `'1:image:_docker_images'` | `arg <image> docker_image` (first positional) |
| `'2::tag:'` (double colon = optional) | `arg [tag]` |
| `'*:file:_files'` | `arg [files...] file` (or `<files...>` if at least one is required) |
| `'(- *)--help[desc]'` | `opt --help "desc"` (the `- *` exclusion of everything is usually not worth modeling) |
| `': :->command'` + `case $state` + `_describe 'command' cmds` | one `cmd <tool> <sub> "desc"` per `'sub:desc'` array entry |
| `'*:: :->args'` + `case $words[1]` | per-subcommand `_arguments` blocks → per-subcommand `cmd` blocks |

Description text: strip escapes like `\[`, `\]`, `\'` and anything after a second `:` in `'sub:desc'`.

## Value actions → types

| zsh action | ezlo |
| --- | --- |
| `_files`, `_path_files` | `file` |
| `_files -/`, `_directories` | `dir` |
| `_files -g '*.json'` | `file` (no glob filter in ezlo; mention the extension in the desc), or a typed generator: `$ ls *.json 2>/dev/null` |
| `(a b c)`, `((a\:desc b\:desc))` | `= a \| b \| c` inline, or `type x` + `values a b c` when reused |
| `_values 'x' a b c` | same as above |
| `_hosts`, `_ssh_hosts` | copy a host type from an existing spec, or parse `~/.ssh/config` (types are file-local) |
| `_users`, `_groups` | `$ getent passwd \| cut -d: -f1` (Linux) / `$ dscl . list /Users` (macOS): pick one per platform or combine with `\|\|` |
| `_pids`, `_process_names` | copy `process`/`process_name` from an existing spec, or generate from `ps` |
| `_command_names` | `$ bash -c 'compgen -c' 2>/dev/null \| sort -u` |
| `_net_interfaces` | `$ ls /sys/class/net 2>/dev/null \|\| ifconfig -l \| tr ' ' '\n'` |
| `_urls`, `_message`, `' '` (empty action) | untyped placeholder (optionally `= "example"` suggestions) |
| custom `_tool_things` function | read its body; port the command it runs (`compadd`/`_describe` arguments) into a `$` generator emitting TSV `value<TAB>desc` with `cols value desc` |

## Gotchas

- zsh completion functions often branch on `$words`/`$CURRENT` or `$opt_args[--foo]` to change the value type: that's a `when` clause (`when --cask: brew_cask`).
- `_call_program tag cmd ...` wraps the real generator command; strip the wrapper.
- `_describe` with two arrays (commands + aliases) → declare the alias as its own `cmd` path with the same body only if it's commonly used; otherwise skip it.
- Big completion files (e.g. `_git`, `_docker`) mirror the whole CLI; port only the subcommands and flags worth completing.

## Example

```zsh
_arguments \
  '(-v --verbose)'{-v,--verbose}'[Verbose output]' \
  '(-q)-n+[Number of lines]:lines:(10 50 100)' \
  '-q[Quiet]' \
  '1:host:_hosts' \
  '*:file:_files'
```

```
cmd mytool "..."
  opt -v --verbose "Verbose output"
  opt -n <lines> = 10 | 50 | 100 "Number of lines" excludes -q
  opt -q "Quiet"
  arg <host> ssh_host
  arg [files...] file
```
