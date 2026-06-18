# ---- builder ----
FROM rust:1-bookworm AS builder
WORKDIR /build
# Pre-build dependencies for layer caching: compile a stub against the manifests,
# then drop in the real sources. (Dependencies are determined by Cargo.toml, not
# by our source, so the stub compiles the full dep graph.)
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release && rm -rf src
COPY src ./src
# Force a rebuild of our crate now that real sources are present.
RUN touch src/main.rs && cargo build --release

# ---- runtime ----
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libssl3 \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /data
COPY --from=builder /build/target/release/agentic-hyperliquid /usr/local/bin/agentic-hyperliquid
ENTRYPOINT ["agentic-hyperliquid"]
