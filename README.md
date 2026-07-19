# salyut-admin

Root administration program for https://salyut.one. It provisions and repairs
accounts, manages the service set, and updates the Git repositories under
`/usr/local/src`.

## Build and test

```sh
make check
make build
```

Common operations are available through the `user`, `services`, and `update`
commands:

```sh
salyut-admin user add rose \
  --signup-email rose@example.com \
  --recovery-email rose-recovery@example.net \
  'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... rose@example'
salyut-admin user repair rose
salyut-admin services health
salyut-admin update
```

Run `salyut-admin --help` or a subcommand's `--help` for the complete command
line interface.

## Deploying

```sh
salyut-admin update
```

## License
[MIT](./LICENSE)
