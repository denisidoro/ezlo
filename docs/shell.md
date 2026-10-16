# Shell Integration & Keys

ezlo is built for Zsh only; other shells aren't supported.

---

## Setup

Add the hook to your `~/.zshrc`:

```sh
eval "$(ezlo init zsh)"
```

The hook starts (or connects to) a background daemon shared by all your shells. It keeps specs, caches and usage memory warm, so each keystroke is a quick request over a Unix socket. You never need to manage it, but `ezlo daemon --status` / `--stop` are there.

ezlo plays well with other Zsh widgets such as zsh-autosuggestions and syntax highlighting: it only draws below the prompt and leaves other widgets' output alone.

---

## Keybindings (Zsh)

| Key | Dropdown open | Only ghost text visible | Otherwise |
|---|---|---|---|
| `Tab` / `Alt-L` | Accept the selected row (opens `ezlo pick` for a recipe) | Normal `Tab` | Show suggestions; on an empty prompt, [recall](learning.md#empty-prompt-recall) this directory's commands |
| `Down` / `Ctrl-N` | Next row (wraps) | Normal | Normal |
| `Up` / `Ctrl-P` | Previous row (wraps) | Normal | Normal |
| `→` / `End` | Normal | At end of line: accept the whole ghost text | Normal |
| `Alt-→` / `Alt-F` | Normal | At end of line: accept the next ghost word | Normal |
| `Enter` | Close the dropdown and **run the line as typed** | Run the line | Run the line |
| `Esc` | Close the dropdown | Normal | Normal (cancels a running translation) |
| `Ctrl-G` | Open [`ezlo pick`](recipes.md) for the current command | Same | Same |
| `Alt-H` | [Explain](line-insight.md#explain-alt-h) the line until the next edit or `Esc` | Same | Same |
| `Alt-P` | Toggle the [preview pane](completion.md#preview-pane-alt-p) for this shell | Same | Same |
| `Shift-Up` / `Shift-Down` | Scroll the preview pane by half its height | Normal | Normal |
| `PageUp` / `PageDown` | While the preview pane is shown: scroll it by half its height; otherwise normal | Normal | Normal |
| `Ctrl-X Ctrl-L` | [Translate](completion.md#comment-lines-natural-language-to-command) the line (a `# comment`) into a command | Same | Same |
| `Alt-K` | Open/close the [settings popup](#settings-popup-alt-k) for this shell | Same | Same |
| `Ctrl-Space` | Normal | Normal | On an empty prompt: recall; otherwise normal |
| `Ctrl-R` | Open [history search](learning.md#history-search-ctrl-r) seeded with the line | Same | Same |

"Normal" means the key keeps whatever it did before ezlo. `Enter` never accepts a suggestion implicitly.

### Changing keys

```zsh
zstyle ':ezlo:keys' pick '^G'
zstyle ':ezlo:keys' explain '^[h'
```

Available names: `up`, `down`, `accept`, `accept-alt` (set to `''` to disable), `cancel`, `ghost-right`, `ghost-end`, `pick`, `popup`, `explain`, `recall`, `pane`, `pane-up`, `pane-down`, `pane-page-up`, `pane-page-down`, `translate`, `search` (set to `''` to keep your own `Ctrl-R`). `toggle` is the old name of `popup` and still works.

For a one-key suggestions on/off switch (what `Alt-K` did before the popup), bind the widget yourself: `bindkey '^[s' _ezlo_toggle`.

`zstyle ':ezlo:preview' max-lines 2` sets how many [effect preview](line-insight.md#effect-preview) lines are kept below an open dropdown (default 2).

`zstyle ':ezlo:recall' empty-tab no` keeps `Tab` on an empty prompt bound to its previous widget (`Ctrl-Space` still recalls).

### Settings popup (`Alt-K`)

`Alt-K` opens a small settings panel below the prompt, aligned to the right edge of the terminal. Changes apply immediately and only to **this shell**: nothing is written to `config.toml`, and a new shell starts from your configuration again.

```text
PROMPT> git checkout ma
                                               ╭─ ezlo settings · this shell ─╮
                                               │ › Suggestions         ‹ on › │
                                               │   Dropdown rows            4 │
                                               │   History rows             3 │
                                               │   Min chars                3 │
                                               │   Ghost text            auto │
                                               │   Signature hint          on │
                                               │   Diagnostics             on │
                                               │   Effect preview          on │
                                               │   Preview pane          auto │
                                               │   Reset to defaults          │
                                               ╰ ↑↓ move  ←→ change  esc ─────╯
```

| Row | Values | Same as |
|---|---|---|
| Suggestions | on / off: the dropdown and ghost text (`Ctrl-G`, `Alt-H` and history recording keep working) | `_ezlo_toggle` |
| Dropdown rows | 1–30 visible rows | `[complete.dropdown] rows`, `$EZLO_MAX_ROWS` |
| History rows | 0–10 history lines mixed into suggestions | `[complete.dropdown] history_rows` |
| Min chars | 0–9 chars of a command name before suggesting | `[complete] min_chars` |
| Ghost text | auto / on / off (auto hides it when zsh-autosuggestions runs) | `$EZLO_GHOST` |
| Signature hint | on / off | `[complete] signature` |
| Diagnostics | on / off: underlines and the `⚠` status line | |
| Effect preview | on / off: effect, value and dry-run lines | `[preview] enabled` |
| Preview pane | auto / on / off | `Alt-P`, `[complete.pane] mode` |
| Reset to defaults | Drops every change made in this shell | |

- **Keys**: `↑`/`↓` (or `k`/`j`, `Ctrl-P`/`Ctrl-N`) move; `←`/`→` (or `h`/`l`, `-`/`+`) change the value; `Space`/`Enter` toggles, cycles or runs the row; `Esc`, `q`, `Ctrl-G` or `Alt-K` close it.
- **The popup is modal**: other keys are ignored while it's open, so the line can't change behind it, and `Enter` acts on the popup instead of running the line. Close it first to keep typing.
- **Defaults** are your configuration's values (the daemon reports them); values you changed are highlighted.
- The dropdown is hidden while the popup is open and comes back, with the new settings, when it closes.

---

## Styling

Colors are set with `zstyle ':ezlo:style' <name> '<spec>'`, using Zsh `region_highlight` syntax (`fg=…`, `bg=…`, `bold`, `underline`):

| Name | Default | Used for |
|---|---|---|
| `selected` | `bg=236` | The selected dropdown row |
| `signature` | `fg=8` | The positional signature hint |
| `signature-active` | `fg=cyan,bold,underline` | The argument currently being filled |
| `detail` | `fg=cyan` | A truncated row's full text in the status line |
| `preview` | `fg=magenta` | Effect lines (`⇢`: blast radius, `cd`, `mkdir`, …) |
| `value` | `fg=green` | Instant value lines (`= /home/you`) |
| `dryrun` | `fg=8` | Spec dry-run output |
| `pane` | `fg=8` | The preview pane's `│` gutter |
| `translate` | `fg=cyan` | The `translating…` status line |
| `translate-error` | `fg=red` | A failed translation's message |
| `scrollbar` | `fg=8` | The dropdown's scrollbar thumb |
| `popup-border` | `fg=8` | The settings popup's frame |
| `popup-changed` | `fg=yellow,bold` | Popup values that differ from the default |

```zsh
zstyle ':ezlo:style' selected 'bg=24,bold'
```

Keys and colors live in `zstyle` (read by the widget without a daemon round-trip); everything else is in [`config.toml`](configuration.md).

---

## History

The Zsh hook sends `$HISTFILE` to the daemon, which reads it live for history rows and ghost text (falling back to `~/.zsh_history` or `~/.histfile`). Executed lines, their directory and exit status are recorded in ezlo's database for [learning](learning.md).
