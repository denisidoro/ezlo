# Installation

ezlo is a single binary plus a one-line Zsh hook. Other shells aren't supported.

---

## Homebrew (macOS & Linux)

The formula lives in this repository ([`Formula/ezlo.rb`](../Formula/ezlo.rb)), so tap the repo by URL:

```sh
brew tap denisidoro/ezlo https://github.com/denisidoro/ezlo
brew install denisidoro/ezlo/ezlo
```

The formula builds from source (Homebrew pulls in Rust as a build-only dependency). To track `main` instead of the latest release, use `brew install --HEAD denisidoro/ezlo/ezlo`.

Update with `brew update && brew upgrade ezlo`; remove with `brew uninstall ezlo && brew untap denisidoro/ezlo`.

## Cargo

Needs a Rust toolchain:

```sh
cargo install ezlo --locked
```

## Prebuilt binaries

Grab an archive for your platform from the [releases page](https://github.com/denisidoro/ezlo/releases) (macOS arm64, Linux x86_64 and arm64), extract it and put `ezlo` on your `$PATH`.

## From source

```sh
git clone https://github.com/denisidoro/ezlo
cd ezlo
cargo install --path . --locked
```

---

## Hook into Zsh

Add this to your `~/.zshrc` and open a new shell:

```sh
eval "$(ezlo init zsh)"
```

ezlo looks best with a [Nerd Font](https://www.nerdfonts.com/) (without one, `export NERD_FONT=0` for ASCII icons). Next steps: [Shell Integration & Keys](shell.md) for keybindings and [Configuration](configuration.md) for tuning.
