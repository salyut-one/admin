use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use clap::Args;

use crate::{process, services};

#[derive(Args)]
pub struct UpdateArgs {
    /// Repository directory names; every eligible repository when omitted.
    repos: Vec<String>,
    /// Parent directory containing the source repositories.
    #[arg(long, default_value = "/usr/local/src")]
    source_root: PathBuf,
}

pub fn update(arguments: &UpdateArgs) -> Result<()> {
    let repositories = select(&arguments.source_root, &arguments.repos)?;
    ensure!(!repositories.is_empty(), "no eligible repositories found");

    for repository in &repositories {
        println!("pulling {}", repository.display());
        process::run(
            "git",
            [
                OsStr::new("-C"),
                repository.as_os_str(),
                OsStr::new("pull"),
                OsStr::new("--ff-only"),
            ],
        )?;
    }
    for repository in &repositories {
        println!("building {}", repository.display());
        process::run("make", [OsStr::new("-C"), repository.as_os_str()])?;
    }
    for repository in &repositories {
        println!("installing {}", repository.display());
        process::run(
            "make",
            [
                OsStr::new("-C"),
                repository.as_os_str(),
                OsStr::new("install"),
            ],
        )?;
    }

    process::run("systemctl", ["daemon-reload"])?;
    services::restart(services::ALL)?;
    services::health(services::ALL)
}

fn select(source_root: &Path, names: &[String]) -> Result<Vec<PathBuf>> {
    ensure!(
        source_root.is_dir(),
        "source root is not a directory: {}",
        source_root.display()
    );

    let mut repositories = if names.is_empty() {
        fs::read_dir(source_root)
            .with_context(|| format!("read {}", source_root.display()))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| format!("read {}", source_root.display()))?
    } else {
        names
            .iter()
            .map(|name| {
                ensure!(
                    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0']),
                    "invalid repository name: {name}"
                );
                Ok(source_root.join(name))
            })
            .collect::<Result<Vec<_>>>()?
    };

    repositories.retain(|repository| eligible(repository));
    repositories.sort();

    if !names.is_empty() && repositories.len() != names.len() {
        let missing = names
            .iter()
            .filter(|name| !eligible(&source_root.join(name)))
            .cloned()
            .collect::<Vec<_>>();
        bail!(
            "not Git repositories with Makefiles under {}: {}",
            source_root.display(),
            missing.join(", ")
        );
    }

    Ok(repositories)
}

fn eligible(repository: &Path) -> bool {
    repository.is_dir() && repository.join(".git").exists() && repository.join("Makefile").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_is_sorted_and_requires_git_and_make() {
        let root = std::env::temp_dir().join(format!(
            "salyut-admin-test-{}-repository-selection",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("site/.git")).unwrap();
        fs::write(root.join("site/Makefile"), "all:\n\ttrue\n").unwrap();
        fs::create_dir_all(root.join("ignored/.git")).unwrap();
        fs::create_dir_all(root.join("bbs/.git")).unwrap();
        fs::write(root.join("bbs/Makefile"), "all:\n\ttrue\n").unwrap();

        assert_eq!(
            select(&root, &[]).unwrap(),
            vec![root.join("bbs"), root.join("site")]
        );
        assert!(select(&root, &["../site".to_owned()]).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
