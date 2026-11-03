# TeamCraft container image.
# Built by the GitLab CI job `teamcraft` with Kaniko (see .gitlab-ci.yml);
# locally: `docker build -t teamcraft .`

# ---------------------------------------------------------------------------
# Build stage
# ---------------------------------------------------------------------------
FROM rust:1-trixie AS builder
WORKDIR /build

# 1) Build the dependencies only, so this layer is cached as long as
#    Cargo.toml / Cargo.lock do not change.
COPY Cargo.toml Cargo.lock build.rs ./
RUN mkdir -p src migrations templates static \
    && echo 'fn main() {}' > src/main.rs \
    && touch src/lib.rs \
    && cargo build --release --locked \
    && rm -rf src

# 2) Build TeamCraft itself. Migrations, templates and static files are
#    embedded into the binary.
COPY migrations ./migrations
COPY templates ./templates
COPY static ./static
COPY src ./src
RUN touch src/main.rs src/lib.rs \
    && cargo build --release --locked \
    && strip target/release/teamcraft

# ---------------------------------------------------------------------------
# Runtime stage
# ---------------------------------------------------------------------------
FROM debian:trixie-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tini \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --user-group --home-dir /data --shell /usr/sbin/nologin teamcraft \
    && mkdir -p /data/vault \
    && chown -R teamcraft:teamcraft /data

COPY --from=builder /build/target/release/teamcraft /usr/local/bin/teamcraft

ENV TEAMCRAFT_BIND_ADDR=0.0.0.0:8080 \
    TEAMCRAFT_VAULT_DIR=/data/vault \
    TEAMCRAFT_LOG_FORMAT=json \
    RUST_LOG=info,tower_http=info,sqlx=warn

USER teamcraft:teamcraft
WORKDIR /data
EXPOSE 8080
VOLUME ["/data/vault"]

HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD ["/usr/local/bin/teamcraft", "healthcheck"]

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/teamcraft"]
CMD ["serve"]

LABEL org.opencontainers.image.title="TeamCraft" \
      org.opencontainers.image.description="Open-source product lifecycle management (PLM)"
