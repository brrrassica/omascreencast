# Contributing to Omascreencast

Thanks for helping. This project is MIT-licensed and stays that way; the rules
below exist to keep it that way and to keep the protocol work honest.

## Clean-room rule (D1) — non-negotiable

The 0.2.0 GameStream host is a **clean-room** implementation. That is a hard
requirement, not a preference.

**Do not transcribe GPL source.** Specifically, do not copy, adapt, paraphrase,
or re-indent any of the following from [Sunshine](https://github.com/LizardByte/Sunshine),
[moonlight-common-c](https://github.com/moonlight-stream/moonlight-common-c), or
any other GPL-licensed GameStream implementation:

- identifiers, function names, or type names;
- comments, or the *layout* of comments and sections;
- struct/field definitions or their packing order;
- cipher, KDF, or handshake logic.

Reference implementations may be run as **black boxes** (the M0 oracle) and
observed over the wire. They may not be read as source to copy from. The
`upstream-sunshine/` checkout is gitignored for exactly this reason; never move
files out of it into the workspace.

If you find that some piece cannot be implemented without copying GPL text,
**stop and open an issue** that re-opens decision D1 in
[`plans/towards-0.2.0.md`](plans/towards-0.2.0.md). A clean-room blocker is a
planning problem, not something to sneak past review.

Protocol constants and field values are acceptable when they come from:

1. a wire trace we captured ourselves (see the trace corpus below), or
2. public, non-GPL protocol documentation,

and both must record their provenance.

## Trace corpus

Captured protocol exchanges live in [`omascreencastd/tests/fixtures/`](omascreencastd/tests/fixtures/).
Every fixture carries provenance — how it was captured, from which client, and
which milestone task it answers. See that directory's `README.md` before adding
or editing a trace. Never hand-edit a captured trace; re-capture it.

## Development

```sh
# python unit tests (includes the M0 spike tooling)
python3 -m unittest discover -s tests -v

# full local gate: manifest, unit tests, headless smoke, validate, qmllint
./scripts/smoke.sh
```

Rust workspace commands (`cargo build`, `cargo test`) apply once
`omascreencastd/` is scaffolded in M1.

## Pull requests

- Keep transport work inside the `gs-*` crates and daemon plumbing inside
  `osc-daemon`; do not leak Hyprland specifics into protocol crates or RTP
  specifics into the daemon.
- Match existing style in the file you touch. There is no separate formatter
  mandate beyond what the file already uses.
- A PR that imports GPL source, comments, or comment structure is rejected, no
  exceptions.
- Update the relevant milestone checkbox in `plans/towards-0.2.0.md` when a
  task lands, and keep that file honest.

## Reporting security issues

See [`SECURITY.md`](SECURITY.md). Do not open a public issue for a
vulnerability.
