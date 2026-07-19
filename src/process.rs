use std::ffi::OsStr;
use std::process::Command;

use anyhow::{Context, Result, ensure};

pub fn run<P, I, S>(program: P, arguments: I) -> Result<()>
where
    P: AsRef<OsStr>,
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let program = program.as_ref();
    let status = Command::new(program)
        .args(arguments)
        .status()
        .with_context(|| format!("run {}", program.to_string_lossy()))?;
    ensure!(
        status.success(),
        "{} failed with {status}",
        program.to_string_lossy()
    );
    Ok(())
}
