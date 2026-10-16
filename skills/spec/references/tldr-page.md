# Porting tldr pages

tldr pages are curated "most common invocations", a good source of recipes (and a sanity check that the grammar covers the common flags). Fetch raw: `https://raw.githubusercontent.com/tldr-pages/tldr/main/pages/<platform>/<cmd>.md` (`common`, `linux`, `osx`).

## Mapping

| tldr | ezlo |
| --- | --- |
| `# cmd` + `> description` | `cmd <tool> "description"` if the root isn't declared yet |
| `- Description:` | `recipe "Description"` (drop the trailing colon; sentence case) |
| `` `cmd {{placeholder}}` `` | `$ cmd <placeholder>` |
| `{{path/to/file}}`, `{{path/to/directory}}` | `<file>`, `<dir>` (the names alone resolve to the builtins) |
| `{{path/to/file1 path/to/file2 ...}}` | template `<files>` + `arg <files...> file` |
| `{{[-f\|--force]}}` (short/long alternatives) | pick the long form in the template |
| `{{8080}}`, `{{username}}` (example values) | `<port>` + `arg <port> = 8080`, `<username>` untyped |

## What to keep

- Keep multi-flag combos and pipelines that are hard to remember (`tar xzvf`, `ffmpeg -ss ... -t ...`, `find ... -exec`).
- Skip entries that are just `cmd <one typed arg>` or `cmd --help`/`--version`: the grammar already completes them.
- Nest each recipe under the most specific declared `cmd` whose path prefixes the template; otherwise top-level.
- Placeholders in a grammar slot (e.g. `<container>` after `docker stop`) get their type inferred; add `arg` lines only for the rest.

## Example

```markdown
- Extract a (compressed) archive file into the current directory verbosely:

`tar xvf {{path/to/source.tar[.gz|.bz2|.xz]}}`
```

```
recipe "Extract an archive into the current directory, verbosely"
  $ tar xvf <archive>
  arg <archive> file
```
