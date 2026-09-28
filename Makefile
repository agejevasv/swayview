PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin
CARGO ?= cargo

BIN := swayview
TARGET := target/release/$(BIN)

.PHONY: all build install uninstall check fmt fmt-check lint test clean

all: build

build:
	$(CARGO) build --release

# Builds only if the binary is missing, so `sudo make install` after `make`
# does not run cargo as root.
$(TARGET):
	$(CARGO) build --release

install: $(TARGET)
	install -Dm755 $(TARGET) $(DESTDIR)$(BINDIR)/$(BIN)

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/$(BIN)

check: fmt-check lint test

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all --check

lint:
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings

test:
	$(CARGO) test --workspace --locked

clean:
	$(CARGO) clean
