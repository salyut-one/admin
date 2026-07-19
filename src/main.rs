mod cli;
mod process;
mod provision;
mod repositories;
mod services;
mod users;

use anyhow::{Result, bail};
use clap::Parser;

fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    // SAFETY: geteuid has no preconditions and does not modify process state.
    if unsafe { libc::geteuid() } != 0 {
        bail!("salyut-admin must run as root");
    }
    cli::execute(cli)
}
