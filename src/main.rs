use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use clap::{Args, Parser, Subcommand};
use xattr::FileExt;

const SERVICES: &[Service] = &[
    Service::systemd("salyut-now", Some(("127.0.0.1:8081", "/healthz"))),
    Service::systemd("salyut-site", Some(("127.0.0.1:8082", "/healthz"))),
    Service::systemd("salyut-bbsd", None),
    Service::systemd("salyut-bbs-web", Some(("127.0.0.1:8080", "/healthz"))),
    Service::systemd("postfix", None),
    Service::systemd("dovecot", None),
    Service::systemd("caddy", None),
];

const USER_GROUP: &str = "salyut-bbs";
const PROFILE_GROUP: &str = "salyut-bbs-web";
const SITE_ROOT: &str = "/srv/user_sites";
const PROFILE_ROOT: &str = "/srv/user_profiles";
const PASSWORD_LEN: usize = 24;
const PASSWORD_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
const RECOVERY_EMAIL_XATTR: &str = "trusted.recovery";
const SIGNUP_EMAIL_XATTR: &str = "trusted.signup";

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: TopCommand,
}

#[derive(Subcommand)]
enum TopCommand {
    /// Create, delete, or repair Salyut users.
    User {
        #[command(subcommand)]
        command: UserCommand,
    },
    /// Inspect, health-check, or restart Salyut services.
    Services {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Pull, build, and install source repositories, then restart services.
    Update(UpdateArgs),
}

#[derive(Subcommand)]
enum UserCommand {
    /// Provision an account and display its generated password once.
    Add {
        username: String,
        ssh_public_key: String,
        /// Email address used to sign up for the account.
        #[arg(long)]
        signup_email: String,
        /// Email address used for account recovery.
        #[arg(long)]
        recovery_email: String,
    },
    /// Display the signup and recovery email addresses for an account.
    Info { username: String },
    /// Remove an account and its Salyut-managed data.
    Delete {
        username: String,
        /// Confirm deletion of the account, home, site, and public profile.
        #[arg(long)]
        yes: bool,
    },
    /// Restore the expected Salyut ownership, modes, files, and links.
    Repair { username: String },
}

#[derive(Subcommand)]
enum ServiceCommand {
    /// Show systemd state for the managed services.
    Status {
        /// Service names; all managed services when omitted.
        services: Vec<String>,
    },
    /// Verify systemd state and application health endpoints.
    Health {
        /// Service names; all managed services when omitted.
        services: Vec<String>,
    },
    /// Restart services and verify their health.
    Restart {
        /// Service names; all managed services when omitted.
        services: Vec<String>,
    },
}

#[derive(Args)]
struct UpdateArgs {
    /// Repository directory names; every eligible repository when omitted.
    repos: Vec<String>,
    /// Parent directory containing the source repositories.
    #[arg(long, default_value = "/usr/local/src")]
    source_root: PathBuf,
}

#[derive(Clone, Copy)]
struct Service {
    name: &'static str,
    http_health: Option<(&'static str, &'static str)>,
}

impl Service {
    const fn systemd(
        name: &'static str,
        http_health: Option<(&'static str, &'static str)>,
    ) -> Self {
        Self { name, http_health }
    }

    fn unit(self) -> String {
        format!("{}.service", self.name)
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    ensure_root()?;

    match cli.command {
        TopCommand::User { command } => match command {
            UserCommand::Add {
                username,
                ssh_public_key,
                signup_email,
                recovery_email,
            } => add_user(&username, &ssh_public_key, &signup_email, &recovery_email),
            UserCommand::Info { username } => user_info(&username),
            UserCommand::Delete { username, yes } => delete_user(&username, yes),
            UserCommand::Repair { username } => repair_user(&username),
        },
        TopCommand::Services { command } => match command {
            ServiceCommand::Status { services } => {
                let selected = select_services(&services)?;
                service_status(&selected)
            }
            ServiceCommand::Health { services } => {
                let selected = select_services(&services)?;
                health_check(&selected)
            }
            ServiceCommand::Restart { services } => {
                let selected = select_services(&services)?;
                restart_services(&selected)?;
                health_check(&selected)
            }
        },
        TopCommand::Update(arguments) => update_repositories(&arguments),
    }
}

fn ensure_root() -> Result<()> {
    // SAFETY: geteuid has no preconditions and does not modify process state.
    if unsafe { libc::geteuid() } != 0 {
        bail!("salyut-admin must run as root");
    }
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
        .status()
        .context("run getent")?;
    match status.code() {
        Some(0) => Ok(true),
        Some(2) => Ok(false),
        _ => bail!("getent passwd failed with {status}"),
    }
}

fn add_user(username: &str, ssh_key: &str, signup_email: &str, recovery_email: &str) -> Result<()> {
    validate_username(username)?;
    validate_ssh_key(ssh_key)?;
    validate_email("signup", signup_email)?;
    validate_email("recovery", recovery_email)?;
    ensure!(
        !account_exists(username)?,
        "account already exists: {username}"
    );

    run("useradd", &["-m", "-G", USER_GROUP, username])?;

    let result: Result<()> = (|| {
        provision_user(username, Some(ssh_key))?;
        store_user_emails(username, signup_email, recovery_email)?;
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

fn user_info(username: &str) -> Result<()> {
    validate_username(username)?;
    ensure!(
        account_exists(username)?,
        "account does not exist: {username}"
    );

    let home = open_home_directory(username)?;
    let signup_email = read_xattr_string(&home, SIGNUP_EMAIL_XATTR, username)?;
    let recovery_email = read_xattr_string(&home, RECOVERY_EMAIL_XATTR, username)?;
    println!("username: {username}");
    println!("signup email: {signup_email}");
    println!("recovery email: {recovery_email}");
    Ok(())
}

fn store_user_emails(username: &str, signup_email: &str, recovery_email: &str) -> Result<()> {
    let home = open_home_directory(username)?;
    home.set_xattr(SIGNUP_EMAIL_XATTR, signup_email.as_bytes())
        .with_context(|| format!("set {SIGNUP_EMAIL_XATTR} for {username}"))?;
    home.set_xattr(RECOVERY_EMAIL_XATTR, recovery_email.as_bytes())
        .with_context(|| format!("set {RECOVERY_EMAIL_XATTR} for {username}"))?;
    Ok(())
}

fn read_xattr_string(file: &File, name: &str, username: &str) -> Result<String> {
    let value = file
        .get_xattr(name)
        .with_context(|| format!("read {name} for {username}"))?
        .ok_or_else(|| anyhow!("{name} is not set for {username}"))?;
    String::from_utf8(value).with_context(|| format!("{name} for {username} is not valid UTF-8"))
}

fn open_home_directory(username: &str) -> Result<File> {
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

fn repair_user(username: &str) -> Result<()> {
    validate_username(username)?;
    ensure!(
        account_exists(username)?,
        "account does not exist: {username}"
    );
    provision_user(username, None)?;
    println!("repaired account: {username}");
    Ok(())
}

fn provision_user(username: &str, ssh_key: Option<&str>) -> Result<()> {
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
        write_file_nofollow(&authorized_keys, format!("{key}\n").as_bytes())?;
    }

    let site = PathBuf::from(SITE_ROOT).join(username);
    ensure_directory(&site, 0o2755, username, username)?;
    let index = site.join("index.html");
    let index_created = ensure_file(&index, 0o755, username, username)?;
    if index_created {
        write_file_nofollow(&index, b"Hello, World!\n")?;
    }
    ensure_symlink(&home.join("public_html"), &site, username, username)?;

    let profiles = PathBuf::from(PROFILE_ROOT);
    ensure_directory(&profiles, 0o711, "root", "root")?;
    let user_profiles = profiles.join(username);
    ensure_directory(&user_profiles, 0o2750, username, PROFILE_GROUP)?;
    for profile in [".plan", ".pgpkey", ".project"] {
        let profile_path = user_profiles.join(profile);
        ensure_file(&profile_path, 0o640, username, PROFILE_GROUP)?;
        ensure_symlink(&home.join(profile), &profile_path, username, username)?;
    }

    run_path(
        Path::new("restorecon"),
        &["-RF".as_ref(), user_profiles.as_os_str()],
    )
}

fn delete_user(username: &str, confirmed: bool) -> Result<()> {
    validate_username(username)?;
    ensure!(
        confirmed,
        "refusing to delete {username}; repeat with --yes to confirm"
    );
    ensure!(
        account_exists(username)?,
        "account does not exist: {username}"
    );

    run("userdel", &["-r", username])?;
    remove_managed_tree(&PathBuf::from(SITE_ROOT).join(username))?;
    remove_managed_tree(&PathBuf::from(PROFILE_ROOT).join(username))?;
    println!("deleted account and managed data: {username}");
    Ok(())
}

fn remove_managed_tree(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

fn rollback_new_user(username: &str) -> Result<()> {
    let mut failures = Vec::new();
    if let Err(error) = run("userdel", &["-r", username]) {
        failures.push(format!("userdel: {error:#}"));
    }
    for root in [SITE_ROOT, PROFILE_ROOT] {
        if let Err(error) = remove_managed_tree(&PathBuf::from(root).join(username)) {
            failures.push(format!("{}: {error:#}", Path::new(root).display()));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        bail!("{}", failures.join("; "))
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

fn write_file_nofollow(path: &Path, contents: &[u8]) -> Result<()> {
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
    let ownership = format!("{owner}:{group}");
    let status = Command::new("chown")
        .arg("-h")
        .arg(ownership)
        .arg(path)
        .status()
        .with_context(|| format!("run chown for {}", path.display()))?;
    ensure!(status.success(), "chown failed for {}", path.display());
    Ok(())
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
    {
        let input = child.stdin.as_mut().context("open chpasswd stdin")?;
        writeln!(input, "{username}:{password}").context("write password to chpasswd")?;
    }
    let status = child.wait().context("wait for chpasswd")?;
    ensure!(status.success(), "chpasswd failed with {status}");
    Ok(())
}

fn select_services(names: &[String]) -> Result<Vec<Service>> {
    if names.is_empty() {
        return Ok(SERVICES.to_vec());
    }
    names
        .iter()
        .map(|name| {
            SERVICES
                .iter()
                .copied()
                .find(|service| service.name == name)
                .ok_or_else(|| anyhow!("unknown managed service: {name}"))
        })
        .collect()
}

fn service_status(services: &[Service]) -> Result<()> {
    let mut arguments = vec![
        "show".to_owned(),
        "--no-pager".to_owned(),
        "--property=Id,LoadState,ActiveState,SubState".to_owned(),
    ];
    arguments.extend(services.iter().map(|service| service.unit()));
    run_owned("systemctl", &arguments)
}

fn restart_services(services: &[Service]) -> Result<()> {
    let mut arguments = vec!["restart".to_owned()];
    arguments.extend(services.iter().map(|service| service.unit()));
    run_owned("systemctl", &arguments)
}

fn health_check(services: &[Service]) -> Result<()> {
    let mut failures = Vec::new();
    for service in services {
        let unit = service.unit();
        let active = Command::new("systemctl")
            .args(["is-active", "--quiet", &unit])
            .status()
            .with_context(|| format!("check {unit}"))?
            .success();
        if !active {
            eprintln!("FAIL {}: systemd unit is not active", service.name);
            failures.push(service.name);
            continue;
        }

        if let Some((address, path)) = service.http_health {
            match http_health(address, path) {
                Ok(()) => println!("ok   {}: active, HTTP healthy", service.name),
                Err(error) => {
                    eprintln!("FAIL {}: {error:#}", service.name);
                    failures.push(service.name);
                }
            }
        } else {
            println!("ok   {}: active", service.name);
        }
    }

    ensure!(
        failures.is_empty(),
        "health check failed: {}",
        failures.join(", ")
    );
    Ok(())
}

fn http_health(address: &str, path: &str) -> Result<()> {
    let socket: SocketAddr = address
        .parse()
        .with_context(|| format!("parse health address {address}"))?;
    let timeout = Duration::from_secs(3);
    let mut stream = TcpStream::connect_timeout(&socket, timeout)
        .with_context(|| format!("connect to http://{address}{path}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .context("set health read timeout")?;
    stream
        .set_write_timeout(Some(timeout))
        .context("set health write timeout")?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .context("write health request")?;

    let mut response = Vec::new();
    stream
        .take(16 * 1024)
        .read_to_end(&mut response)
        .context("read health response")?;
    let response = String::from_utf8_lossy(&response);
    let status = response.lines().next().unwrap_or_default();
    ensure!(
        status.starts_with("HTTP/1.1 200 ") || status.starts_with("HTTP/1.0 200 "),
        "http://{address}{path} returned {status:?}"
    );
    Ok(())
}

fn update_repositories(arguments: &UpdateArgs) -> Result<()> {
    let repositories = select_repositories(&arguments.source_root, &arguments.repos)?;
    ensure!(!repositories.is_empty(), "no eligible repositories found");

    for repository in &repositories {
        println!("pulling {}", repository.display());
        run_path(
            Path::new("git"),
            &[
                "-C".as_ref(),
                repository.as_os_str(),
                "pull".as_ref(),
                "--ff-only".as_ref(),
            ],
        )?;
    }
    for repository in &repositories {
        println!("building {}", repository.display());
        run_path(Path::new("make"), &["-C".as_ref(), repository.as_os_str()])?;
    }
    for repository in &repositories {
        println!("installing {}", repository.display());
        run_path(
            Path::new("make"),
            &["-C".as_ref(), repository.as_os_str(), "install".as_ref()],
        )?;
    }

    run("systemctl", &["daemon-reload"])?;
    restart_services(SERVICES)?;
    health_check(SERVICES)
}

fn select_repositories(source_root: &Path, names: &[String]) -> Result<Vec<PathBuf>> {
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

    repositories.retain(|repository| {
        repository.is_dir()
            && repository.join(".git").exists()
            && repository.join("Makefile").is_file()
    });
    repositories.sort();

    if !names.is_empty() && repositories.len() != names.len() {
        let missing = names
            .iter()
            .filter(|name| {
                let path = source_root.join(name);
                !(path.is_dir() && path.join(".git").exists() && path.join("Makefile").is_file())
            })
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

fn run(program: &str, arguments: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(arguments)
        .status()
        .with_context(|| format!("run {program}"))?;
    ensure!(status.success(), "{program} failed with {status}");
    Ok(())
}

fn run_owned(program: &str, arguments: &[String]) -> Result<()> {
    let status = Command::new(program)
        .args(arguments)
        .status()
        .with_context(|| format!("run {program}"))?;
    ensure!(status.success(), "{program} failed with {status}");
    Ok(())
}

fn run_path(program: &Path, arguments: &[&std::ffi::OsStr]) -> Result<()> {
    let status = Command::new(program)
        .args(arguments)
        .status()
        .with_context(|| format!("run {}", program.display()))?;
    ensure!(
        status.success(),
        "{} failed with {status}",
        program.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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
    fn user_add_requires_both_email_options() {
        let parsed = Cli::try_parse_from([
            "salyut-admin",
            "user",
            "add",
            "rose",
            "ssh-ed25519 AAAA rose@example",
            "--signup-email",
            "rose@example.com",
            "--recovery-email",
            "rose-recovery@example.net",
        ])
        .unwrap();
        assert!(matches!(
            parsed.command,
            TopCommand::User {
                command: UserCommand::Add { .. }
            }
        ));

        assert!(
            Cli::try_parse_from([
                "salyut-admin",
                "user",
                "add",
                "rose",
                "ssh-ed25519 AAAA rose@example",
                "--signup-email",
                "rose@example.com",
            ])
            .is_err()
        );
    }

    #[test]
    fn parses_user_info_command() {
        let parsed = Cli::try_parse_from(["salyut-admin", "user", "info", "rose"]).unwrap();
        assert!(matches!(
            parsed.command,
            TopCommand::User {
                command: UserCommand::Info { username }
            } if username == "rose"
        ));
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

    #[test]
    fn service_selection_defaults_to_all_and_rejects_unknown_names() {
        assert_eq!(select_services(&[]).unwrap().len(), SERVICES.len());
        assert_eq!(
            select_services(&["caddy".to_owned()]).unwrap()[0].name,
            "caddy"
        );
        assert!(select_services(&["sshd".to_owned()]).is_err());
    }

    #[test]
    fn repository_selection_is_sorted_and_requires_git_and_make() {
        let root = std::env::temp_dir().join(format!(
            "salyut-admin-test-{}-{}",
            std::process::id(),
            "repository-selection"
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("site/.git")).unwrap();
        fs::write(root.join("site/Makefile"), "all:\n\ttrue\n").unwrap();
        fs::create_dir_all(root.join("ignored/.git")).unwrap();
        fs::create_dir_all(root.join("bbs/.git")).unwrap();
        fs::write(root.join("bbs/Makefile"), "all:\n\ttrue\n").unwrap();

        let selected = select_repositories(&root, &[]).unwrap();
        assert_eq!(selected, vec![root.join("bbs"), root.join("site")]);
        assert!(select_repositories(&root, &["../site".to_owned()]).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
