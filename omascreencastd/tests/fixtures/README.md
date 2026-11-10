# Trace corpus (M0-T7)

Golden protocol traces for the GameStream host. These are not inventable from
memory: each fixture is a **capture** from a real client talking to a real
server, committed so protocol crates can be replayed against them byte-for-byte
(M1-T11, M2-T9).

> **Empty until the M0 spikes run.** The directory tree below is the agreed
> layout and provenance contract. Its fixtures are populated by M0-T5 when the
> capture steps in [`../../../docs/m0-runbook.md`](../../../docs/m0-runbook.md)
> are executed against the oracle (M0-T1) and the fake server (M0-T2). Do not
> hand-write a trace to fill a gap.

## Layout

```
omascreencastd/tests/fixtures/
  m0/
    spike-capture.jsonl        # default output of tools/m0-gamestream/fake_server.py
    oracle-*.jsonl             # captures from the real Sunshine oracle (black box)
    README.md                  # per-capture provenance table for this drop
  http/                        # /serverinfo, /applist, /launch, /resume, /cancel
  pairing/                     # pairing known-answer vectors (PIN, salt, challenges)
  rtsp/                        # DESCRIBE / SETUP / PLAY / TEARDOWN frames
  control/                     # control-channel handshake + ping + termination
  video/                       # first RTP video packets after PLAY
```

## HTTP fixture format (M1-T11)

The `http/` directory is consumed by the replay harness in
[`crates/gs-http/src/replay.rs`](../../crates/gs-http/src/replay.rs) and the
integration test [`crates/gs-http/tests/golden_traces.rs`](../../crates/gs-http/tests/golden_traces.rs).
Each `*.jsonl` file holds one capture; every non-empty, non-`#` line is one JSON
object.

A file may open with a **server directive** describing the host the capture was
taken against. Every fixture in the file is then replayed against a state built
from it, so asserted fields match the capture rather than a hard-coded test host:

```json
{"server": {"hostname": "captured-host", "mac": "00:11:22:33:44:55", "local_ip": "192.168.1.2", "https_port": 47984, "rtsp_port": 48010}}
```

Each subsequent line is one **exchange**:

```json
{
  "name": "serverinfo-unpaired",
  "request": {
    "method": "GET",
    "path": "/serverinfo",
    "query": "",
    "headers": [["Host", "captured-host"]],
    "body": "",
    "address": "192.168.1.40",
    "fingerprint": null,
    "certificate_der_hex": null
  },
  "expect": {
    "status": 200,
    "content_type": "application/json",
    "json": {"hostname": "captured-host", "PairStatus": 1}
  }
}
```

Rules for the `expect` block:

- `status` is required; `content_type`, `json`, and `body` are optional.
- `json` is a **subset** match: every key present must match, extra keys in the
  response are ignored. Use it for responses with volatile fields (a `uniqueid`
  derived from the certificate, a random challenge) and assert only the parts
  the protocol fixes.
- `body` is an exact byte match, for responses the protocol fixes completely.
- `certificate_der_hex` is the client certificate in DER form, hex-encoded, for
  fixtures that exercise a paired route.

The harness skips cleanly while the directory holds no `*.jsonl` files, so CI is
green before the capture lands and starts asserting the moment it does.

## Pairing fixture format (M1-T11)

The `pairing/` directory is consumed by the replay harness in
[`crates/gs-crypto/src/replay.rs`](../../crates/gs-crypto/src/replay.rs) and the
integration test [`crates/gs-crypto/tests/golden_traces.rs`](../../crates/gs-crypto/tests/golden_traces.rs).
Each `*.jsonl` file holds one capture; every non-empty, non-`#` line is one JSON
object. A pairing fixture is a **known-answer vector**: the inputs the client and
host exchanged, plus the values the protocol fixes for them.

```json
{
  "name": "phase2-server-proof",
  "pin": "1234",
  "salt_hex": "00112233445566778899aabbccddeeff",
  "client_challenge_hex": "0a0b0c0d0e0f10111213141516171819",
  "server_challenge_hex": "1a1b1c1d1e1f20212223242526272829",
  "expect": {
    "pin_key_hex": "…",
    "server_proof_hex": "…",
    "client_proof_hex": "…"
  }
}
```

Rules for the `expect` block:

- Every field is optional, but at least one must be present; a fixture that
  asserts nothing is refused rather than counted as a pass.
- `pin_key_hex` asserts the PIN-derived key for `pin` + `salt_hex`.
- `server_proof_hex` asserts the host's proof of `client_challenge_hex`, and
  requires `client_challenge_hex` to be present.
- `client_proof_hex` asserts the value the client must return for
  `server_challenge_hex`, and requires `server_challenge_hex` to be present.
- All byte fields are lowercase hex. Redact nothing here: a pairing vector is
  only useful if its inputs are the real captured bytes.

The harness skips cleanly while the directory holds no `*.jsonl` files, so CI is
green before the capture lands and starts asserting the moment it does.

## Provenance contract

Every captured file needs a row in its directory's `README.md` (or a sibling
`.provenance.json`) recording, at minimum:

| Field | Meaning |
|---|---|
| `captured_at` | UTC timestamp of the capture |
| `client` | Moonlight app + platform + version (e.g. Android TV 0.x.y) |
| `server` | `fake_server.py` (this repo) or `sunshine@<commit>` (oracle) |
| `task` | Which M0 task the trace answers (T3/T4/T5) |
| `method` | How it was captured (proxy, `tcpdump`, server log, …) |
| `tool` | Exact command used, or a pointer to it in the runbook |
| `redacted` | Fields removed or masked (keys, PINs, MACs, IPs) |

Traces must be **verbatim wire bytes** apart from the redaction list. If a
secret must be removed, record that it was removed, never synthesize a plausible
replacement.

## Regenerating

See [`../../../docs/m0-runbook.md`](../../../docs/m0-runbook.md) for the exact
commands. In short:

```sh
# fake-server capture (no hardware needed)
python3 tools/m0-gamestream/fake_server.py --log omascreencastd/tests/fixtures/m0/spike-capture.jsonl

# oracle capture (needs the Android TV + Sunshine oracle, M0-T1)
# follow the runbook's tcpdump / proxy recipe; the runbook owns the command.
```

## Rules

- **Never hand-edit a capture.** Re-capture it and update the provenance row.
- **Never commit keys, PINs, or pairing secrets.** Redact and record the
  redaction.
- Keep captures small and purposeful: the exchange a task needs, not a whole
  session, unless a full session is what the task is about (M2-T9).
- Fixtures are consumed by golden tests, so their bytes are a contract. A change
  to a fixture means a protocol change or a re-capture — say which in the commit.
