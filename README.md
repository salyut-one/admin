# Salyut admin

`salyut-admin` is the root-only administration program for salyut.one. It
replaces `/root/salyut-manage.sh`, preserves that script's account, website,
SSH, and public-profile provisioning contract, and implements the service and
source-update operations described by SAL-21.

## Build and install

```sh
make check
make build
sudo make install
```

The default install path is `/sbin/salyut-admin`, as required by SAL-21. Set
`SBINDIR` when packaging into a staged or locally managed prefix.

## Commands

Create an account and print its generated password once:

```sh
sudo salyut-admin user add rose \
  --signup-email 'rose@example.com' \
  --recovery-email 'rose-recovery@example.net' \
  'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... rose@example'
```

The signup and recovery addresses are stored on the user's home directory as
the root-only `trusted.signup` and `trusted.recovery` extended attributes.
Retrieve them with:

```sh
sudo salyut-admin user info rose
```

Set or replace both addresses for an existing account:

```sh
sudo salyut-admin user info set \
  --signup 'rose@example.com' \
  --recovery 'rose-recovery@example.net' \
  rose
```

Repair ownership, modes, missing files, and profile links without replacing
user content:

```sh
sudo salyut-admin user repair rose
```

Delete an account and its Salyut site/profile trees:

```sh
sudo salyut-admin user delete rose --yes
```

Inspect, check, or restart the managed services:

```sh
sudo salyut-admin services status
sudo salyut-admin services health
sudo salyut-admin services restart
sudo salyut-admin services restart salyut-now caddy
```

Update every Git repository with a Makefile under `/usr/local/src`:

```sh
sudo salyut-admin update
sudo salyut-admin update bbs now site
```

An update pulls every selected repository with `git pull --ff-only`, builds
all of them before installing any, installs them, reloads systemd, restarts all
managed services, and runs the full health check. The build-first ordering
prevents a compile failure from leaving a partially installed release.

Service health combines `systemctl is-active` for all seven services with
their application health endpoints where available:

- `salyut-bbs-web` at `127.0.0.1:8080/healthz`, which also exercises
  `salyut-bbsd`
- `salyut-now` at `127.0.0.1:8081/healthz`
- `salyut-site` at `127.0.0.1:8082/healthz`

The managed service set is `salyut-now`, `salyut-site`, `salyut-bbsd`,
`salyut-bbs-web`, `postfix`, `dovecot`, and `caddy`.
