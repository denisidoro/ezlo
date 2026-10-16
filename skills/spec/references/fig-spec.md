# Porting Fig specs (`withfig/autocomplete`)

ezlo's model was inspired by Fig, so its specs are the most direct source. Files live at `withfig/autocomplete/src/<cmd>.ts` (subcommand-heavy tools may split into `src/<cmd>/*.ts`). Fetch raw: `https://raw.githubusercontent.com/withfig/autocomplete/master/src/<cmd>.ts`.

## Mapping

| Fig | ezlo |
| --- | --- |
| `name: "docker"`, `description` | `cmd docker "description"` |
| `subcommands: [{ name: "run", ... }]` | `cmd docker run "..."` (recurse for nested subcommands) |
| `name: ["rm", "remove"]` (aliases) | one `cmd` per alias only if both are common; otherwise the canonical name |
| `options: [{ name: ["-f", "--force"], description }]` | `opt -f --force "description"` |
| option `args: { name: "file" }` | `opt ... <file>` |
| option `isRepeatable: true` + args | `<value...>` |
| option `isPersistent: true` | `global` |
| option `isRequired: true` | `required` |
| option `exclusiveOn: ["-q"]` | `excludes -q` |
| option `requiresSeparator: true` (`--opt=value`) | nothing special (ezlo parses `--opt=value`) |
| top-level `args: { name, isOptional, isVariadic }` | `arg <name>`, `arg [name]`, `<name...>` |
| `args: [a, b]` (array) | one `arg` line each, in order |
| `template: "filepaths"` / `"folders"` / both | `file` / `dir` / `path` |
| `suggestions: ["a", "b"]` or `[{ name: "a", description }]` | `= a \| b`, or a `values` type, or a `$ printf` generator with a desc column |
| `generators: { script: "cmd", postProcess }` | `type` with `$ cmd`; re-implement `postProcess` (usually `split("\n").map(...)`) in `awk`/`jq` so the generator emits TSV |
| `generators: { script: (tokens) => [...] }` | params: tokens referring to flags → `opt.<name>`, to earlier args → `arg.<name>` |
| `cache: { ttl: 30000 }` | `cache 30s` |
| `generators: { custom: async ... }` | read the logic; port it to a shell pipeline or skip |
| `loadSpec: "other"` | the subcommand accepts another command line (`sudo`, `time`): ezlo handles common wrappers via `[line] wrappers`; otherwise `arg [command...]` |
| `icon`, `priority`, `hidden`, `deprecated` | drop (skip hidden/deprecated entries entirely) |

## Gotchas

- Fig specs are often generated from `--help` and include every flag; trim to what's worth completing.
- Shared helpers (`const containersGenerator = ...`) are reused across subcommands: port them once as a `type` and reference it.
- `postProcess` often parses JSON (`JSON.parse(out)`): use `--format '{{...}}\t...'` or `jq -r '... | @tsv'` instead.
