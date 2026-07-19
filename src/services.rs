use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, ensure};

use crate::process;

pub const ALL: &[Service] = &[
    Service::systemd("salyut-site", Some(("127.0.0.1:8082", "/healthz"))),
    Service::systemd("salyut-bbsd", None),
    Service::systemd("postfix", None),
    Service::systemd("dovecot", None),
    Service::systemd("caddy", None),
];

#[derive(Clone, Copy)]
pub struct Service {
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

pub fn select(names: &[String]) -> Result<Vec<Service>> {
    if names.is_empty() {
        return Ok(ALL.to_vec());
    }
    names
        .iter()
        .map(|name| {
            ALL.iter()
                .copied()
                .find(|service| service.name == name)
                .ok_or_else(|| anyhow!("unknown managed service: {name}"))
        })
        .collect()
}

pub fn status(services: &[Service]) -> Result<()> {
    let mut arguments = vec![
        "show".to_owned(),
        "--no-pager".to_owned(),
        "--property=Id,LoadState,ActiveState,SubState".to_owned(),
    ];
    arguments.extend(services.iter().map(|service| service.unit()));
    process::run("systemctl", arguments)
}

pub fn restart(services: &[Service]) -> Result<()> {
    let mut arguments = vec!["restart".to_owned()];
    arguments.extend(services.iter().map(|service| service.unit()));
    process::run("systemctl", arguments)
}

pub fn health(services: &[Service]) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_defaults_to_all_and_rejects_unknown_names() {
        assert_eq!(select(&[]).unwrap().len(), ALL.len());
        assert_eq!(select(&["caddy".to_owned()]).unwrap()[0].name, "caddy");
        assert!(select(&["sshd".to_owned()]).is_err());
    }
}
