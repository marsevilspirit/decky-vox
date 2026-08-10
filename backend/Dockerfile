FROM ghcr.io/steamdeckhomebrew/holo-toolchain-rust@sha256:818de45e147f35798c66498380b31bd1fe8bf75afdf0a22b027053706836faf1

RUN set -eu; \
    attempt=1; \
    until rustup toolchain install 1.94.0 \
      --profile minimal \
      --component clippy \
      --component rustfmt \
      --target x86_64-unknown-linux-gnu; do \
        if [ "${attempt}" -ge 3 ]; then exit 1; fi; \
        attempt=$((attempt + 1)); \
    done

ENTRYPOINT ["/backend/entrypoint.sh"]
