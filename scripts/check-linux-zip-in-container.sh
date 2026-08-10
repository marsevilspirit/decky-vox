#!/usr/bin/env bash
set -euo pipefail

ZIP_PATH="${1:?ZIP path is required}"
EXTRACT_DIR="$(mktemp -d /tmp/decky-vox-zip.XXXXXX)"
trap 'rm -rf -- "${EXTRACT_DIR}"' EXIT

bsdtar -xf "${ZIP_PATH}" -C "${EXTRACT_DIR}"

for relative in bin/decky-vox-core bin/voxtype bin/voxtype-vulkan; do
  binary="${EXTRACT_DIR}/decky-vox/${relative}"
  test -x "${binary}"
  file "${binary}"
  readelf -h "${binary}" | grep -Eq 'Class:[[:space:]]+ELF64'
  readelf -h "${binary}" | grep -Eq 'Machine:[[:space:]]+Advanced Micro Devices X86-64'

  case "${relative}" in
    bin/decky-vox-core)
      allowed_steamos_missing=""
      ;;
    bin/voxtype)
      allowed_steamos_missing="libasound.so.2"
      ;;
    bin/voxtype-vulkan)
      allowed_steamos_missing="libasound.so.2 libvulkan.so.1"
      ;;
  esac

  if dependencies="$(ldd "${binary}" 2>&1)"; then
    if grep -Fq 'not found' <<<"${dependencies}"; then
      while read -r missing; do
        [[ -n "${missing}" ]] || continue
        if [[ " ${allowed_steamos_missing} " != *" ${missing} "* ]]; then
          echo "Unexpected missing dynamic dependency ${missing} for ${relative}:" >&2
          echo "${dependencies}" >&2
          exit 1
        fi
        echo "${relative}: ${missing} is an expected SteamOS runtime library; real Deck verification remains required"
      done < <(awk '/=> not found/{print $1}' <<<"${dependencies}")
    fi
    echo "${dependencies}"
  elif ! grep -Eq 'not a dynamic executable|statically linked' <<<"${dependencies}"; then
    echo "Unable to inspect dynamic dependencies for ${relative}:" >&2
    echo "${dependencies}" >&2
    exit 1
  fi
done
