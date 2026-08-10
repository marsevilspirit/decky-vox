.PHONY: all test clean

all:
	cargo build --release --locked --target x86_64-unknown-linux-gnu
	install -Dm755 target/x86_64-unknown-linux-gnu/release/decky-vox-core out/decky-vox-core

test:
	cargo test --all-targets --locked --target x86_64-unknown-linux-gnu

clean:
	cargo clean
	rm -f out/decky-vox-core
