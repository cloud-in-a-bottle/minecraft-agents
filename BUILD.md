# Building minecraft-agents (Rust/azalea)

Pinned to **azalea 0.15.1 (+mc1.21.11)**. Two things make the build reproducible and
must not be casually changed: the **nightly toolchain** and the **crypto pins in `Cargo.lock`**.

## Quickstart

```bash
# from rust/
cargo build --locked            # dev build; --locked enforces the pinned Cargo.lock
cargo run --locked              # run locally (needs ANTHROPIC_API_KEY etc. — see ../README.md)
```

If you don't already have the toolchain, `rustup` installs it automatically from
`rust-toolchain.toml` on the first cargo command (~200 MB, one time).

## Why nightly

azalea → `simdnbt`, which uses `#![feature(portable_simd)]` (nightly-only, no stable
fallback). `rust-toolchain.toml` pins **`nightly-2026-02-03`** — azalea 0.15.1's release-day
nightly. Don't float to plain `nightly`: current nightly has drifted months past this and
breaks azalea/Bevy.

## Why the crypto pins (the important part)

azalea 0.15.1 depends on **pre-release** RustCrypto crates. Resolving fresh today pulls the
*final* releases of those crates — which were published **after** azalea 0.15.1 and changed
APIs (e.g. `pkcs8::Error::KeyMalformed` unit → tuple, `der::SequenceRef` lost a lifetime),
so azalea no longer compiles against them. `Cargo.lock` pins the exact cohort azalea shipped
with (from azalea's own release-commit lockfile):

| crate | pin | | crate | pin |
|---|---|---|---|---|
| `rsa` | `0.10.0-rc.13` | | `der` | `0.8.0-rc.10` |
| `signature` | `3.0.0-rc.8` | | `spki` | `0.8.0-rc.4` |
| `pkcs8` | `0.11.0-rc.9` | | `pkcs1` | `0.8.0-rc.4` |

`rsa`/`signature`/`crypto-bigint`/`const-oid` resolve correctly on their own (azalea-auth
pins `signature = "=3.0.0-rc.8"`, which anchors the rest). The three that get wrongly upgraded
to finals are **`pkcs8`, `spki`, `der`**.

### If you add or bump a dependency

Adding a crate to `Cargo.toml` makes cargo re-resolve and re-grab the broken finals. After any
dep change, re-apply the pins:

```bash
cargo update -p pkcs8 --precise 0.11.0-rc.9
cargo update -p spki  --precise 0.8.0-rc.4
cargo update -p der   --precise 0.8.0-rc.10
cargo build --locked                          # confirm it still compiles
```

Then commit the updated `Cargo.lock`. **Add crates in `Cargo.toml` here, not ad hoc** — the
manifest + lock are the single source of truth for the dependency graph.

## Deploy (OpenHost)

`../Dockerfile` builds this crate. Nothing is cross-compiled or replicated by hand: the
builder installs the pinned nightly (via `rust-toolchain.toml`) and runs `cargo build
--release --locked` (honoring the pins), then ships a slim `debian` image with just the
stripped binary + ca-certs (rustls TLS + bundled sqlite → no OpenSSL, no system sqlite, no Java).

```bash
git push
oh app reload minecraft-agents --update --wait   # or: oh app deploy <repo> --name minecraft-agents --wait
```

The dep compile (azalea + Bevy) is heavy on the first build (minutes, multi-GB RAM) but caches
as its own image layer, so code-only redeploys are fast. The builder needs normal crates.io
egress; the committed `Cargo.lock` means it never re-resolves.
