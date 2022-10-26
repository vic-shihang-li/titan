.PHONY: all clean

CARGO_HOME=$(CURDIR)/.cargo
RUSTUP_HOME=$(CURDIR)/opt/rust

all: node

clean:
	rm -f ./node

node:
	RUSTUP_HOME=$(RUSTUP_HOME) rustup override set nightly && \
	CARGO_HOME=$(CARGO_HOME) cargo +nightly build --release --bin node && \
	cp target/release/node ./node

