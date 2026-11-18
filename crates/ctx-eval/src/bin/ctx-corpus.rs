//! Command-line tools for the decision point evaluation corpus.

use std::process::ExitCode;

use clap::Parser;
use ctx_eval::corpus::Cli;

fn main() -> ExitCode {
    match Cli::parse().run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
