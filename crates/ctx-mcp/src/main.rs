use std::{env, process::ExitCode};

use ctx_adapters::git::GitRepo;

fn main() -> ExitCode {
    let result = env::current_dir()
        .map_err(ctx_mcp::McpServerError::Io)
        .and_then(|current| {
            GitRepo::discover(&current)
                .map_err(ctx_mcp::McpServerError::Git)
                .and_then(|git| ctx_mcp::serve_stdio(&git))
        });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ctx-mcp: {error}");
            ExitCode::FAILURE
        }
    }
}
