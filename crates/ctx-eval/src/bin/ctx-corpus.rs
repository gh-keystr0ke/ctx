//! Command-line tools for the decision point evaluation corpus.

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    match std::env::args().skip(1).collect::<Vec<_>>().as_slice() {
        [command] if command == "check" => {
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus");
            match ctx_eval::corpus::check(&root) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("{error}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!("Usage: ctx-corpus check");
            ExitCode::from(2)
        }
    }
}
