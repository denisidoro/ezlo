mod drift;
mod probe;
mod render;
mod sandbox;
mod scrape;

pub use drift::{detect_drift, probe_help, render_drift, AddedFlag, AddedSubcmd, CmdDrift, HelpFetcher, MAX_PROBED_PATHS};
pub use probe::{check_path_allowed, resolve_binary, run_sandboxed_probe};
pub use render::render_spec_file;
pub use sandbox::run_internal_probe_exec;
pub use scrape::parse_help_text;

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::PathBuf;

use crate::config::Config;
use crate::spec::{has_errors, parse_and_validate};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrapedOpt {
    pub flags: Vec<String>,
    pub value_placeholder: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrapedSubcmd {
    pub name: String,
    pub description: Option<String>,
    pub opts: Vec<ScrapedOpt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrapedHelp {
    pub description: Option<String>,
    pub opts: Vec<ScrapedOpt>,
    pub subcommands: Vec<ScrapedSubcmd>,
}

pub fn import_command(cmd_name: &str) -> Result<(PathBuf, ScrapedHelp)> {
    let binary = resolve_binary(cmd_name)?;

    let help_output = ["--help", "-h", "help"]
        .into_iter()
        .filter_map(|flag| run_sandboxed_probe(&binary, &[flag]).ok())
        .find(|out| !out.trim().is_empty());

    let text = help_output.with_context(|| format!("failed to obtain help output from `{cmd_name}`"))?;
    let mut scraped = parse_help_text(&text);

    // Probe up to 8 top-level subcommands for their options
    for sub in scraped.subcommands.iter_mut().take(8) {
        if let Ok(sub_out) = run_sandboxed_probe(&binary, &[&sub.name, "--help"]) {
            sub.opts = parse_help_text(&sub_out).opts;
        }
    }

    let spec_text = render_spec_file(cmd_name, &scraped);
    let (_, diags) = parse_and_validate(&spec_text);
    if has_errors(&diags) {
        bail!("generated spec failed validation: {:?}", diags);
    }

    let out_dir = Config::generated_specs_dir();
    fs::create_dir_all(&out_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&out_dir, fs::Permissions::from_mode(0o700));
    }
    let out_path = out_dir.join(format!("{cmd_name}.{}", crate::spec::SPEC_EXT));
    fs::write(&out_path, &spec_text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&out_path, fs::Permissions::from_mode(0o600));
    }

    Ok((out_path, scraped))
}
