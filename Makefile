CARGO ?= cargo
INSTALL ?= install
SBINDIR ?= /sbin

.PHONY: all build test check install

all: build

build:
	$(CARGO) build --release --locked

test:
	$(CARGO) test --locked

check:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --all-targets --locked -- -D warnings
	$(CARGO) test --locked

install:
	$(INSTALL) -d "$(DESTDIR)$(SBINDIR)"
	$(INSTALL) -m 0755 target/release/salyut-admin \
		"$(DESTDIR)$(SBINDIR)/salyut-admin"
