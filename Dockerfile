# Rust/azalea build. The crate lives at the repo root; rust-toolchain.toml pins nightly-2026-02-03
# (azalea needs nightly for simdnbt), and Cargo.lock pins the RustCrypto pre-releases that
# make azalea 0.15.1 resolve. Build honors both with --locked.
FROM rust:1-bookworm AS builder
WORKDIR /app
# Toolchain + manifests first so the heavy dep compile (azalea + Bevy) caches across src edits.
COPY rust-toolchain.toml ./rust-toolchain.toml
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src target/release/deps/minecraft_agents* target/release/minecraft-agents
# Real sources; only this layer rebuilds on code changes.
COPY src ./src
RUN cargo build --release --locked

# Slim runtime: azalea uses rustls + bundled sqlite, so only CA certs are needed (HTTPS to the LLM APIs).
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/minecraft-agents /usr/local/bin/minecraft-agents
ENV PORT=8080
EXPOSE 8080
CMD ["minecraft-agents"]
