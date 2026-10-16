//! zsh adapter: the full ZLE integration (`shell/ezlo.zsh`) talks to the daemon over its socket
//! (`daemon::protocol`), so `ezlo init` only emits the script. `native.rs` is the daemon's
//! fallback for commands without a spec: a persistent `zsh -f` worker running `_complete`.

pub mod native;

const SCRIPT: &str = include_str!("../../shell/ezlo.zsh");

pub fn init_script() -> String {
    SCRIPT.to_owned()
}
