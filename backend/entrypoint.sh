#!/bin/sh
set -eu

# Decky CLI runs backend containers as the invoking host user. The pinned
# toolchain is preinstalled read-only in the image; Cargo's registry/cache must
# still live in writable container scratch space.
export RUSTUP_HOME=/.rustup
export CARGO_HOME=/tmp/decky-vox-cargo
mkdir -p "${CARGO_HOME}"

cd /backend
make all
