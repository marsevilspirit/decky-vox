#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
DECKY_CLI="${DECKY_CLI:-${REPO_DIR}/cli/decky}"
EXPECTED_DECKY_CLI_VERSION="decky 0.0.8"

if [[ ! -x "${DECKY_CLI}" ]]; then
  echo "Decky CLI not found or not executable: ${DECKY_CLI}" >&2
  echo "Install a pinned Decky CLI at cli/decky or set DECKY_CLI." >&2
  exit 2
fi

if [[ "$("${DECKY_CLI}" --version)" != "${EXPECTED_DECKY_CLI_VERSION}" ]]; then
  echo "Decky CLI version mismatch; expected ${EXPECTED_DECKY_CLI_VERSION}." >&2
  exit 2
fi

cd "${REPO_DIR}"

pnpm install --frozen-lockfile
pnpm test
"${SCRIPT_DIR}/test-backend-linux.sh"

"${DECKY_CLI}" plugin build . \
  --output-path ./out \
  --tmp-output-path /tmp/decky-vox-build \
  --output-filename-source directory

python3 "${SCRIPT_DIR}/check_zip.py" "${REPO_DIR}/out/decky-vox.zip"
"${SCRIPT_DIR}/check-linux-zip.sh" "${REPO_DIR}/out/decky-vox.zip"
