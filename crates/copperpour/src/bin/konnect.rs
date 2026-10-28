//! `konnect`, the name Copperpour had before the rename. It runs the
//! `copperpour` executable beside it with the same arguments, so MCP client
//! configurations and hooks that name `konnect` keep working for one release.

use std::path::PathBuf;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let target = match copperpour_beside_this_executable() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("konnect: cannot find copperpour: {error}");
            return ExitCode::FAILURE;
        }
    };
    // stderr only: stdout carries the MCP protocol and hook output.
    eprintln!(
        "konnect: this program is now called copperpour; running {}",
        target.display()
    );
    let mut command = Command::new(&target);
    command.args(std::env::args_os().skip(1));
    run(command, &target)
}

fn copperpour_beside_this_executable() -> std::io::Result<PathBuf> {
    let current = std::env::current_exe()?;
    let directory = current.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "executable has no directory")
    })?;
    let target = directory.join(format!("copperpour{}", std::env::consts::EXE_SUFFIX));
    if target.is_file() {
        Ok(target)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{} does not exist", target.display()),
        ))
    }
}

/// Replace this process, so signals and the client's pipes reach copperpour
/// directly.
#[cfg(unix)]
fn run(mut command: Command, target: &std::path::Path) -> ExitCode {
    use std::os::unix::process::CommandExt;
    let error = command.exec();
    eprintln!("konnect: cannot run {}: {error}", target.display());
    ExitCode::FAILURE
}

/// Windows has no exec: wait for copperpour, sharing this process's handles,
/// and exit with its code.
#[cfg(not(unix))]
fn run(mut command: Command, target: &std::path::Path) -> ExitCode {
    match command.status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(error) => {
            eprintln!("konnect: cannot run {}: {error}", target.display());
            ExitCode::FAILURE
        }
    }
}
