#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
TOOLCHAIN_IMAGE="ghcr.io/steamdeckhomebrew/holo-toolchain-rust@sha256:818de45e147f35798c66498380b31bd1fe8bf75afdf0a22b027053706836faf1"

docker info >/dev/null

docker run --rm \
  --platform linux/amd64 \
  --entrypoint /bin/bash \
  --mount "type=bind,source=${REPO_DIR}/backend,target=/backend,readonly" \
  --mount "type=volume,target=/backend/target" \
  --workdir /backend \
  "${TOOLCHAIN_IMAGE}" \
  -lc 'cargo fmt --all -- --check && cargo clippy --all-targets --all-features --locked -- -D warnings && cargo test --all-targets --locked'
