#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
ZIP_PATH="${1:-${REPO_DIR}/out/decky-vox.zip}"
TOOLCHAIN_IMAGE="ghcr.io/steamdeckhomebrew/holo-toolchain-rust@sha256:818de45e147f35798c66498380b31bd1fe8bf75afdf0a22b027053706836faf1"

if [[ "${ZIP_PATH}" != /* ]]; then
  ZIP_DIRECTORY="$(cd -- "$(dirname -- "${ZIP_PATH}")" && pwd)"
  ZIP_PATH="${ZIP_DIRECTORY}/$(basename -- "${ZIP_PATH}")"
fi

if [[ ! -f "${ZIP_PATH}" ]]; then
  echo "ZIP does not exist: ${ZIP_PATH}" >&2
  exit 2
fi

case "${ZIP_PATH}" in
  "${REPO_DIR}"/*) ;;
  *)
    echo "ZIP must be inside the repository so it can be mounted read-only." >&2
    exit 2
    ;;
esac

RELATIVE_ZIP="${ZIP_PATH#${REPO_DIR}/}"
docker info >/dev/null

docker run --rm \
  --platform linux/amd64 \
  --entrypoint /bin/bash \
  --mount "type=bind,source=${REPO_DIR},target=/workspace,readonly" \
  "${TOOLCHAIN_IMAGE}" \
  /workspace/scripts/check-linux-zip-in-container.sh "/workspace/${RELATIVE_ZIP}"
