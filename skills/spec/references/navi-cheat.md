# Porting navi `.cheat` files

navi cheats map mostly to `recipe` nodes, and their `$ var:` lines map to `type` nodes. Local cheats live in `navi/cheats/*.cheat`; community ones in `denisidoro/cheats` and `denisidoro/navi-tldr-pages`.

## Mapping

| navi | ezlo |
| --- | --- |
| `% docker, container` (tags) | which file the recipes go to (`docker.ezlo`); no ezlo equivalent for tags |
| `# Description` + command line(s) | `recipe "Description"` + `$ command` |
| multi-line command ending in `\` | same `\` continuation inside the `$` line |
| `<var>` in the command | `<var>` placeholder (rename to lowercase snake_case: `<image-id>` → `<image_id>`) |
| `$ var: shell command` | `type <tool>_<var>` + `$ shell command` (name it after what it lists, not the navi var) |
| `$ var: echo "a b c" \| tr ' ' '\n'` (static list) | `arg <var> = a \| b \| c` on the recipe, or `type` + `values a b c` |
| `--- --column 2` | `cols ...` naming every column + `insert <col 2>` |
| `--- --delimiter ';'` / `column -t -s';'` | make the generator emit TSV instead (`tr ';' '\t'`, `awk -v OFS='\t'`); drop `column -t` |
| `--- --header-lines 1` | strip the header in the generator (`tail -n +2`, `awk 'NR > 1'`, or a `--format` without headers) |
| `--- --multi` | template `<var>` + `arg <var...> type` (variadic, `Space` multi-selects) |
| `--- --preview 'cmd {}'` | `preview \`cmd "$col"\`` on the type (`{}` → the column var, `{2}` → the 2nd column's var) |
| `--- --map 'cmd'` | apply the transform inside the generator so `insert` gets the final value |
| `--- --fzf-overrides`, `--query`, `--prevent-extra` | drop |
| `${other_var}` used inside a `$ var:` line | type param: `type x(other = arg.other_var)` and `$other` in the shell (fills `<other_var>` first) |
| `@ other_cheat` (extend) | copy the needed types into this file (types are file-local) |
| `; comment` / `;` metadata | drop (or keep as a `#` comment if it's informative) |

## Workflow

1. For each snippet, check whether it's already expressible as grammar: `docker stop <container_id>` adds nothing if `cmd docker stop` has `arg <container> docker_container`; skip it or improve the grammar instead.
2. Recipes that start with a spec'd command path go nested under that `cmd` (the template must start with the full path, `E08`); everything else becomes a top-level `recipe`, grouped at the end of the themed file.
3. Drop `arg` lines whose placeholder is already typed by the grammar slot it occupies; keep them only to override or add suggestions.
4. Replace navi-era helpers that won't exist on the target host (personal scripts, `brew cask list`) with direct, portable commands.
5. Delete or trim the ported snippets from the `.cheat` file only if asked; navi and ezlo can coexist.

## Example

```
% docker

# Run a shell inside a running container
docker exec -it <container> <shell>

$ container: docker ps --format '{{.ID}}\t{{.Names}}\t{{.Image}}' --- --column 2 --preview 'docker logs --tail 20 {1}'
$ shell: echo "bash sh" | tr ' ' '\n'
```

```
type docker_container
  $ docker ps --format '{{.ID}}\t{{.Names}}\t{{.Image}}' 2>/dev/null
  cols id name image
  insert name
  preview `docker logs --tail 20 "$id" 2>&1`
  cache 2s

cmd docker exec "Execute a command in a running container"
  ...
  recipe "Run a shell inside a running container"
    $ docker exec -it <container> <shell>
    arg <container> docker_container
    arg <shell> = bash | sh
```
