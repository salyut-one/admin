use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail, ensure};
use xattr::FileExt;

use crate::{process, provision};

const PASSWORD_LEN: usize = 24;
const PASSWORD_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
const RECOVERY_EMAIL_XATTR: &str = "trusted.recovery";
const SIGNUP_EMAIL_XATTR: &str = "trusted.signup";

pub fn add(username: &str, ssh_key: &str, signup_email: &str, recovery_email: &str) -> Result<()> {
    validate_username(username)?;
    validate_ssh_key(ssh_key)?;
    validate_email("signup", signup_email)?;
    validate_email("recovery", recovery_email)?;
    ensure!(
        !account_exists(username)?,
        "account already exists: {username}"
    );

    process::run("useradd", ["-m", "-G", provision::USER_GROUP, username])?;

    let result: Result<()> = (|| {
        provision::apply(username, Some(ssh_key))?;
        store_emails(username, signup_email, recovery_email)?;
        let password = generate_password()?;
        set_password(username, &password)?;
        println!("created account: {username}");
        println!("password: {password}");
        Ok(())
    })();

    if let Err(error) = result {
        let rollback = rollback_new_user(username);
        return match rollback {
            Ok(()) => Err(error.context("provisioning failed; the new account was rolled back")),
            Err(rollback_error) => Err(error.context(format!(
                "provisioning failed and rollback also failed: {rollback_error:#}"
            ))),
        };
    }
    Ok(())
}

pub fn info(username: &str) -> Result<()> {
    require_account(username)?;
    let home = open_home(username)?;
    let signup = read_xattr(&home, SIGNUP_EMAIL_XATTR, username)?
        .ok_or_else(|| anyhow!("{SIGNUP_EMAIL_XATTR} is not set for {username}"))?;
    let recovery = read_xattr(&home, RECOVERY_EMAIL_XATTR, username)?;
    println!("username: {username}");
    println!("signup email: {signup}");
    println!(
        "recovery email: {}",
        recovery.as_deref().unwrap_or("(not set)")
    );
    Ok(())
}

pub fn set_emails(username: &str, signup: &str, recovery: Option<&str>) -> Result<()> {
    validate_username(username)?;
    validate_email("signup", signup)?;
    if let Some(recovery) = recovery {
        validate_email("recovery", recovery)?;
    }
    ensure!(
        account_exists(username)?,
        "account does not exist: {username}"
    );

    let home = open_home(username)?;
    let previous_signup = home
        .get_xattr(SIGNUP_EMAIL_XATTR)
        .with_context(|| format!("read {SIGNUP_EMAIL_XATTR} for {username}"))?;
    home.set_xattr(SIGNUP_EMAIL_XATTR, signup.as_bytes())
        .with_context(|| format!("set {SIGNUP_EMAIL_XATTR} for {username}"))?;

    if let Some(recovery) = recovery
        && let Err(error) = home.set_xattr(RECOVERY_EMAIL_XATTR, recovery.as_bytes())
    {
        let rollback = restore_xattr(&home, SIGNUP_EMAIL_XATTR, previous_signup.as_deref());
        return match rollback {
            Ok(()) => Err(error).with_context(|| {
                format!("set {RECOVERY_EMAIL_XATTR} for {username}; restored previous signup email")
            }),
            Err(rollback_error) => Err(error).with_context(|| {
                format!(
                    "set {RECOVERY_EMAIL_XATTR} for {username}; failed to restore previous signup email: {rollback_error:#}"
                )
            }),
        };
    }

    println!("updated email addresses: {username}");
    Ok(())
}

pub fn repair(username: &str) -> Result<()> {
    require_account(username)?;
    provision::apply(username, None)?;
    println!("repaired account: {username}");
    Ok(())
}

pub fn delete(username: &str, confirmed: bool) -> Result<()> {
    validate_username(username)?;
    ensure!(
        confirmed,
        "refusing to delete {username}; repeat with --yes to confirm"
    );
    ensure!(
        account_exists(username)?,
        "account does not exist: {username}"
    );

    process::run("userdel", ["-r", username])?;
    provision::remove_managed_data(username)?;
    println!("deleted account and managed data: {username}");
    Ok(())
}

fn require_account(username: &str) -> Result<()> {
    validate_username(username)?;
    ensure!(
        account_exists(username)?,
        "account does not exist: {username}"
    );
    Ok(())
}

fn validate_username(username: &str) -> Result<()> {
    let mut chars = username.chars();
    let first = chars.next().ok_or_else(|| anyhow!("username is empty"))?;
    ensure!(
        first.is_ascii_lowercase() || first == '_',
        "invalid username: {username}"
    );
    ensure!(
        username.len() <= 32
            && chars.all(|character| {
                character.is_ascii_lowercase()
                    || character.is_ascii_digit()
                    || character == '_'
                    || character == '-'
            }),
        "invalid username: {username}"
    );
    Ok(())
}

fn validate_ssh_key(key: &str) -> Result<()> {
    ensure!(!key.trim().is_empty(), "SSH public key is empty");
    ensure!(
        !key.contains(['\n', '\r', '\0']),
        "SSH public key must be one line"
    );
    Ok(())
}

fn validate_email(label: &str, email: &str) -> Result<()> {
    ensure!(!email.is_empty(), "{label} email address is empty");
    ensure!(
        !email.contains(['\n', '\r', '\0']),
        "{label} email address must be one line"
    );
    Ok(())
}

fn account_exists(username: &str) -> Result<bool> {
    let status = Command::new("getent")
        .args(["passwd", username])
        .stdout(Stdio::null())
        .status()
        .context("run getent")?;
    match status.code() {
        Some(0) => Ok(true),
        Some(2) => Ok(false),
        _ => bail!("getent passwd failed with {status}"),
    }
}

fn store_emails(username: &str, signup: &str, recovery: &str) -> Result<()> {
    let home = open_home(username)?;
    home.set_xattr(SIGNUP_EMAIL_XATTR, signup.as_bytes())
        .with_context(|| format!("set {SIGNUP_EMAIL_XATTR} for {username}"))?;
    home.set_xattr(RECOVERY_EMAIL_XATTR, recovery.as_bytes())
        .with_context(|| format!("set {RECOVERY_EMAIL_XATTR} for {username}"))
}

fn restore_xattr(file: &File, name: &str, previous: Option<&[u8]>) -> std::io::Result<()> {
    match previous {
        Some(value) => file.set_xattr(name, value),
        None => file.remove_xattr(name),
    }
}

fn read_xattr(file: &File, name: &str, username: &str) -> Result<Option<String>> {
    file.get_xattr(name)
        .with_context(|| format!("read {name} for {username}"))?
        .map(String::from_utf8)
        .transpose()
        .with_context(|| format!("{name} for {username} is not valid UTF-8"))
}

fn open_home(username: &str) -> Result<File> {
    let home = PathBuf::from("/home").join(username);
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(&home)
        .with_context(|| {
            format!(
                "open home directory without following links: {}",
                home.display()
            )
        })?;
    ensure!(
        directory.metadata()?.file_type().is_dir(),
        "expected home directory does not exist: {}",
        home.display()
    );
    Ok(directory)
}

fn rollback_new_user(username: &str) -> Result<()> {
    let mut failures = Vec::new();
    if let Err(error) = process::run("userdel", ["-r", username]) {
        failures.push(format!("userdel: {error:#}"));
    }
    provision::remove_managed_data_for_rollback(username, &mut failures);
    if failures.is_empty() {
        Ok(())
    } else {
        bail!("{}", failures.join("; "))
    }
}

fn generate_password() -> Result<String> {
    let mut random = File::open("/dev/urandom").context("open /dev/urandom")?;
    password_from_reader(&mut random, PASSWORD_LEN)
}

fn password_from_reader(reader: &mut impl Read, length: usize) -> Result<String> {
    ensure!(length > 0, "password length must be positive");
    let cutoff = u8::MAX - (u8::MAX % PASSWORD_ALPHABET.len() as u8);
    let mut password = String::with_capacity(length);
    let mut byte = [0_u8; 1];
    while password.len() < length {
        reader.read_exact(&mut byte).context("read random bytes")?;
        if byte[0] < cutoff {
            password.push(PASSWORD_ALPHABET[byte[0] as usize % PASSWORD_ALPHABET.len()] as char);
        }
    }
    Ok(password)
}

fn set_password(username: &str, password: &str) -> Result<()> {
    let mut child = Command::new("chpasswd")
        .stdin(Stdio::piped())
        .spawn()
        .context("start chpasswd")?;
    writeln!(
        child.stdin.as_mut().context("open chpasswd stdin")?,
        "{username}:{password}"
    )
    .context("write password to chpasswd")?;
    let status = child.wait().context("wait for chpasswd")?;
    ensure!(status.success(), "chpasswd failed with {status}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn accepts_script_compatible_usernames() {
        for username in ["a", "_service", "rose-2", "user_name"] {
            validate_username(username).unwrap();
        }
    }

    #[test]
    fn rejects_unsafe_usernames() {
        for username in ["", "Root", "2user", "../root", "a.b", &"a".repeat(33)] {
            assert!(validate_username(username).is_err(), "{username:?}");
        }
    }

    #[test]
    fn validates_email_values_for_xattrs() {
        for email in ["rose@example.com", "rose+recovery@example.net"] {
            validate_email("test", email).unwrap();
        }
        for email in [
            "",
            "rose@example.com\nsecond@example.com",
            "rose\0@example.com",
        ] {
            assert!(validate_email("test", email).is_err(), "{email:?}");
        }
    }

    #[test]
    fn deterministic_password_generation_rejects_out_of_range_bytes() {
        let mut input = Cursor::new([255, 254, 0, 1, 2, 3]);
        let password = password_from_reader(&mut input, 4).unwrap();
        assert_eq!(password.len(), 4);
        assert_eq!(
            password,
            String::from_utf8(PASSWORD_ALPHABET[..4].to_vec()).unwrap()
        );
    }
}
