//! Runs the canonical native desktop shell against an explicitly disposable profile.
//! Build with `--features native-smoke`; never a shipped application command.
use anyhow::{Context, Result, ensure};
use std::{path::PathBuf, process::ExitCode};

fn run() -> Result<i32> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    // WorkerClient intentionally relaunches its current executable. Preserve the
    // real startup guard and worker protocol instead of recursively opening UI.
    if arguments == [std::ffi::OsString::from("_worker")] {
        return brotypist::cli::run(vec!["_worker".into()]);
    }
    #[cfg(target_os = "macos")]
    if arguments.len() == 2 && arguments[0] == "--layout-only" {
        return brotypist::ui::macos::capture_layout(&PathBuf::from(&arguments[1]));
    }
    ensure!(
        arguments.len() == 1,
        "Usage: native_desktop_smoke <absolute-output-directory>"
    );
    let output = PathBuf::from(
        arguments
            .into_iter()
            .next()
            .context("Missing output directory")?,
    );
    brotypist::ui::smoke(&output)
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            eprintln!("BroTypist native desktop smoke: {error:#}");
            ExitCode::FAILURE
        }
    }
}
