# omascreencastd

The 0.2.0 GameStream host: a Moonlight-compatible replacement for the Chromecast
transport. This crate tree is the Rust engine described in
[`../plans/towards-0.2.0.md`](../plans/towards-0.2.0.md) §2.

**This is a clean-room implementation.** Do not copy, transcribe, or paraphrase
Sunshine or any other GPL GameStream implementation. See
[`../THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md) and
[`../CONTRIBUTING.md`](../CONTRIBUTING.md) before touching this tree.

## Status

| Crate | Purpose | Landed in |
|---|---|---|
| `gs-proto` | Wire constants, endian helpers, packed structs, XML/JSON DTOs | M1-T2 |
| `gs-crypto` | Certificate identity, pairing state machine, trust store | M1-T3, M1-T4 (partial) |
| `gs-http` | GameStream HTTPS server and routing | stub |
| `gs-rtsp` | RTSP parser/serialiser and SDP | stub |
| `gs-stream` | Control channel and RTP packetisation | stub |
| `gs-mdns` | `_nvstream._tcp` advertisement | stub |
| `gs-capture` | Capture backends behind one trait | stub |
| `gs-encode` | Encoder backends behind one trait | stub |
| `osc-daemon` | Session manager, state, IPC, subcommands | scaffold |
| `xtask` | Release and fixture helpers | scaffold |

Stub crates exist so the workspace layout and CI job are fixed early; each gains
its implementation in the milestone named in the plan.

## Toolchain

- Pinned in [`rust-toolchain.toml`](rust-toolchain.toml): `1.99.1`.
- MSRV floor declared in [`Cargo.toml`](Cargo.toml): Rust `1.85` (edition 2024).
- Workspace lints forbid `unsafe_code` and warn on missing docs and `Debug`
  impls; CI turns warnings into errors.

## Commands

Run from this directory (the workspace root):

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo build --all-features
cargo run -p osc-daemon -- identity   # create/load the server identity, print fingerprint
```

`cargo xtask` is wired up for release and fixture helpers; it currently prints
its own roadmap.

## Identity storage (D9)

The server certificate and key live outside the plugin folder, under
`$XDG_DATA_HOME/omascreencast/gamestream/` (`~/.local/share/...` when
`XDG_DATA_HOME` is unset):

| File | Mode | Contents |
|---|---|---|
| `cert.pem` | `0644` | Self-signed RSA-2048 server certificate |
| `key.pem` | `0600` | Matching private key |

The identity is generated once on first run and never silently regenerated: an
incomplete pair (one file present) is an error, because replacing half of it
would invalidate every paired client. `clients.json` lands here with M1-T8.

## Pairing (M1-T4, partial)

[`crates/gs-crypto/src/pairing/`](crates/gs-crypto/src/pairing/) implements the
protocol shell: the four phases, explicit out-of-order rejection, a five-minute
TTL with a bounded pending store, PIN validation, and the `pairing_pending` /
`pairing_result` IPC events from plan §5.1.

The **byte-level** KDF and challenge construction are deliberately behind the
`PairingCrypto` trait. `ReferenceCrypto` is a provisional, self-consistent
implementation used to exercise the state machine in tests; it is **not**
expected to interoperate with Moonlight. The M0 trace corpus (M0-T5) pins the
real bytes, at which point a production implementation replaces it without
touching the state machine.
