use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

use crate::process;

pub const USER_GROUP: &str = "salyut-bbs";
const PROFILE_GROUP: &str = USER_GROUP;
const SITE_ROOT: &str = "/srv/user_sites";
const PROFILE_ROOT: &str = "/srv/user_profiles";

pub fn apply(username: &str, ssh_key: Option<&str>) -> Result<()> {
    let home = PathBuf::from("/home").join(username);
    let home_metadata =
        fs::symlink_metadata(&home).with_context(|| format!("inspect {}", home.display()))?;
    ensure!(
        home_metadata.file_type().is_dir(),
        "expected home directory does not exist: {}",
        home.display()
    );

    let ssh_dir = home.join(".ssh");
    ensure_directory(&ssh_dir, 0o700, username, username)?;
    let authorized_keys = ssh_dir.join("authorized_keys");
    ensure_file(&authorized_keys, 0o600, username, username)?;
    if let Some(key) = ssh_key {
        write_file(&authorized_keys, format!("{key}\n").as_bytes())?;
    }

    let site = PathBuf::from(SITE_ROOT).join(username);
    ensure_directory(&site, 0o2755, username, username)?;
    let index = site.join("index.html");
    if ensure_file(&index, 0o755, username, username)? {
        write_file(&index, b"Hello, World!\n")?;
    }
    ensure_symlink(&home.join("public_html"), &site, username, username)?;

    let profiles = PathBuf::from(PROFILE_ROOT);
    ensure_directory(&profiles, 0o711, "root", "root")?;
    let user_profiles = profiles.join(username);
    ensure_directory(&user_profiles, 0o2750, username, PROFILE_GROUP)?;
    for profile in [".plan", ".project"] {
        let profile_path = user_profiles.join(profile);
        ensure_file(&profile_path, 0o640, username, PROFILE_GROUP)?;
        ensure_symlink(&home.join(profile), &profile_path, username, username)?;
    }

    process::run("restorecon", ["-RF".as_ref(), user_profiles.as_os_str()])
}

pub fn remove_managed_data(username: &str) -> Result<()> {
    for root in [SITE_ROOT, PROFILE_ROOT] {
        remove_tree(&PathBuf::from(root).join(username))?;
    }
    Ok(())
}

pub fn remove_managed_data_for_rollback(username: &str, failures: &mut Vec<String>) {
    for root in [SITE_ROOT, PROFILE_ROOT] {
        if let Err(error) = remove_tree(&PathBuf::from(root).join(username)) {
            failures.push(format!("{}: {error:#}", Path::new(root).display()));
        }
    }
}

fn remove_tree(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

fn ensure_directory(path: &Path, mode: u32, owner: &str, group: &str) -> Result<()> {
    if !path.exists() {
        fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
    }
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("open directory without following links: {}", path.display()))?;
    ensure!(
        directory.metadata()?.file_type().is_dir(),
        "{} is not a directory",
        path.display()
    );
    directory
        .set_permissions(fs::Permissions::from_mode(mode))
        .with_context(|| format!("chmod {:04o} {}", mode, path.display()))?;
    set_owner(path, owner, group)
}

fn ensure_file(path: &Path, mode: u32, owner: &str, group: &str) -> Result<bool> {
    let (file, created) = match OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => (file, false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW)
                .create_new(true)
                .open(path)
                .with_context(|| format!("create {}", path.display()))?;
            (file, true)
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("open file without following links: {}", path.display()));
        }
    };
    ensure!(
        file.metadata()?.file_type().is_file(),
        "{} is not a regular file",
        path.display()
    );
    file.set_permissions(fs::Permissions::from_mode(mode))
        .with_context(|| format!("chmod {:04o} {}", mode, path.display()))?;
    set_owner(path, owner, group)?;
    Ok(created)
}

fn write_file(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("open file without following links: {}", path.display()))?;
    file.write_all(contents)
        .with_context(|| format!("write {}", path.display()))
}

fn ensure_symlink(link: &Path, target: &Path, owner: &str, group: &str) -> Result<()> {
    match fs::read_link(link) {
        Ok(existing) => ensure!(
            existing == target,
            "{} points to {}, expected {}",
            link.display(),
            existing.display(),
            target.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            symlink(target, link)
                .with_context(|| format!("link {} to {}", link.display(), target.display()))?;
        }
        Err(error) => return Err(error).with_context(|| format!("inspect {}", link.display())),
    }
    set_owner(link, owner, group)
}

fn set_owner(path: &Path, owner: &str, group: &str) -> Result<()> {
    process::run(
        "chown",
        [
            "-h".as_ref(),
            format!("{owner}:{group}").as_ref(),
            path.as_os_str(),
        ],
    )
    .with_context(|| format!("set owner of {}", path.display()))
}
