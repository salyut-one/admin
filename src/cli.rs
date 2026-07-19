use anyhow::{Result, bail};
use clap::{Parser, Subcommand};

use crate::{
    repositories::{self, UpdateArgs},
    services, users,
};

#[derive(Parser)]
#[command(version, about)]
pub struct Cli {
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
    /// Display or set the signup and recovery email addresses for an account.
    Info {
        #[command(subcommand)]
        command: Option<UserInfoCommand>,
        /// Account to inspect when no info subcommand is given.
        username: Option<String>,
    },
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
enum UserInfoCommand {
    /// Set the signup and recovery email addresses for an existing account.
    Set {
        #[arg(long)]
        signup: String,
        #[arg(long)]
        recovery: Option<String>,
        username: String,
    },
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

pub fn execute(cli: Cli) -> Result<()> {
    match cli.command {
        TopCommand::User { command } => match command {
            UserCommand::Add {
                username,
                ssh_public_key,
                signup_email,
                recovery_email,
            } => users::add(&username, &ssh_public_key, &signup_email, &recovery_email),
            UserCommand::Info { command, username } => match (command, username) {
                (None, Some(username)) => users::info(&username),
                (
                    Some(UserInfoCommand::Set {
                        signup,
                        recovery,
                        username,
                    }),
                    None,
                ) => users::set_emails(&username, &signup, recovery.as_deref()),
                (None, None) => bail!("username is required"),
                (Some(_), Some(_)) => bail!("username must follow the info subcommand"),
            },
            UserCommand::Delete { username, yes } => users::delete(&username, yes),
            UserCommand::Repair { username } => users::repair(&username),
        },
        TopCommand::Services { command } => match command {
            ServiceCommand::Status { services: names } => {
                services::status(&services::select(&names)?)
            }
            ServiceCommand::Health { services: names } => {
                services::health(&services::select(&names)?)
            }
            ServiceCommand::Restart { services: names } => {
                let selected = services::select(&names)?;
                services::restart(&selected)?;
                services::health(&selected)
            }
        },
        TopCommand::Update(arguments) => repositories::update(&arguments),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                command: UserCommand::Info {
                    command: None,
                    username: Some(username),
                }
            } if username == "rose"
        ));
    }

    #[test]
    fn parses_user_info_set_command() {
        let parsed = Cli::try_parse_from([
            "salyut-admin",
            "user",
            "info",
            "set",
            "--signup",
            "rose@example.com",
            "--recovery",
            "rose-recovery@example.net",
            "rose",
        ])
        .unwrap();
        assert!(matches!(
            parsed.command,
            TopCommand::User {
                command: UserCommand::Info {
                    command: Some(UserInfoCommand::Set { username, .. }),
                    username: None,
                }
            } if username == "rose"
        ));

        let parsed = Cli::try_parse_from([
            "salyut-admin",
            "user",
            "info",
            "set",
            "--signup",
            "rose@example.com",
            "rose",
        ])
        .unwrap();
        assert!(matches!(
            parsed.command,
            TopCommand::User {
                command: UserCommand::Info {
                    command: Some(UserInfoCommand::Set { recovery: None, .. }),
                    username: None,
                }
            }
        ));
    }
}
