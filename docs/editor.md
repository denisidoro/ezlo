# Editor Support

ezlo ships a small VS Code extension, in [`extensions/syntax/`](../extensions/syntax/), that highlights `.ezlo` spec files. It's only a grammar: no language server, linter or extra binary.

---

## What you get

- Keywords (`type`, `cmd`, `opt`, `arg`, `recipe`, `when`, `values`, `preview`, …), flags, `<required>`/`[optional]` placeholders, `"strings"` and `#` comments.
- Shell highlighting inside `$ …` generator/template lines (including `\` continuations) and `` `backtick` `` previews.
- `#` toggles comments, brackets and quotes auto-close, and blocks fold by indentation.

It works in VS Code and in editors that install VS Code extensions from a `.vsix` file (VSCodium, Cursor, …).

---

## Build

You need [Node.js](https://nodejs.org/) (for `npx`). From a checkout of the repo:

```sh
cd extensions/syntax
npx @vscode/vsce package
```

This writes `ezlo-syntax-<version>.vsix` next to `package.json`. The extension's version matches ezlo's.

---

## Install

```sh
code --install-extension extensions/syntax/ezlo-syntax-*.vsix
```

Or in VS Code: open the Extensions view, click `…` → **Install from VSIX…**, and pick the file. Use `codium` or `cursor` instead of `code` for those editors.

To update, build and install the new `.vsix` again. To remove it: `code --uninstall-extension denisidoro.ezlo-syntax`.

---

## Use

Open any `*.ezlo` file (for example under `~/.config/ezlo/specs/`): highlighting applies automatically. For a spec saved with another extension, pick **ezlo** from the language picker in the status bar, or map it in your settings:

```json
"files.associations": {
  "*.myspec": "ezlo"
}
```

Highlighting doesn't validate anything: run [`ezlo check`](spec-tooling.md#ezlo-check-validate-specs) for real diagnostics.
