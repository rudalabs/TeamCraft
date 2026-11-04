# syntax=docker/dockerfile:1
#
# Container image of the TeamCraft server: ghcr.io/rudalabs/teamcraft.
#
# Built and published by GitHub Actions only (.github/workflows/release.yml),
# natively on an amd64 and an arm64 runner. Run it with deploy/docker-compose.yml
# (docs/src/admin/docker.md). Configuration comes from environment variables
# and *_FILE secrets only (docs/src/admin/configuration.md).

# ---- build -----------------------------------------------------------------
FROM rust:1-slim-bookworm AS builder
WORKDIR /src

# Dependencies first, in their own layer: it is rebuilt only when Cargo.toml
# or Cargo.lock change. The dummy sources are replaced below.
COPY Cargo.toml Cargo.lock build.rs ./
RUN mkdir src \
 && echo 'fn main() {}' > src/main.rs \
 && touch src/lib.rs \
 && cargo build --release --locked --bin teamcraft \
 && rm -rf src

COPY src src
COPY migrations migrations
COPY templates templates
COPY static static
COPY media media
# `touch`: COPY keeps the file times, which can be older than the dummy build.
RUN touch src/main.rs src/lib.rs \
 && cargo build --release --locked --bin teamcraft \
 && mkdir -p /out/var/lib/teamcraft/vault \
 && cp target/release/teamcraft /out/teamcraft

# ---- runtime ---------------------------------------------------------------
# glibc, libgcc, CA certificates and tzdata, no shell, user "nonroot" (65532).
FROM gcr.io/distroless/cc-debian12:nonroot

ARG VERSION=dev
LABEL org.opencontainers.image.title="TeamCraft" \
      org.opencontainers.image.description="Open-source PDM/PLM server: items, revisions, files, BOMs, change orders" \
      org.opencontainers.image.source="https://github.com/rudalabs/TeamCraft" \
      org.opencontainers.image.url="https://rudalabs.github.io/TeamCraft/" \
      org.opencontainers.image.documentation="https://rudalabs.github.io/TeamCraft/admin/docker.html" \
      org.opencontainers.image.licenses="MPL-2.0" \
      org.opencontainers.image.version="${VERSION}"

COPY --from=builder /out/teamcraft /usr/local/bin/teamcraft
# The vault (uploaded files and cached previews) – mount a volume here.
COPY --from=builder --chown=65532:65532 /out/var/lib/teamcraft /var/lib/teamcraft

ENV TEAMCRAFT_BIND_ADDR=0.0.0.0:8080 \
    TEAMCRAFT_VAULT_DIR=/var/lib/teamcraft/vault

USER 65532:65532
VOLUME ["/var/lib/teamcraft"]
EXPOSE 8080

# `teamcraft healthcheck` probes /healthz on TEAMCRAFT_BIND_ADDR.
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
  CMD ["/usr/local/bin/teamcraft", "healthcheck"]

ENTRYPOINT ["/usr/local/bin/teamcraft"]
CMD ["serve"]
