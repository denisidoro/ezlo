#![forbid(unsafe_code)]

use std::process;

fn main() {
    if let Err(err) = ezlo::cli::run_cli() {
        eprintln!("ezlo: {err:#}");
        process::exit(1);
    }
}
