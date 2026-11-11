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
{"server": {"hostname": "captured-host", "mac": "00:11:22:33:44:55", "local_ip": "192.168.1.2", "https_port": 47984, "rtsp_port": 48010, "paired": false}}
```

`paired` defaults to `false` and says whether the captured host had at least
one paired client — the captures differ (`moonlight-discovery.jsonl` was a
never-paired host with `PairStatus: 0`; `oracle-unpaired.jsonl` had one, with
`PairStatus: 1`), and the replayed `/serverinfo` must agree with whichever was
captured.

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

- `status` is **required**, but note that a captured GameStream host answers
  `200` even for a protocol failure: the real result is `xml.status_code`.
- `xml` asserts the body, the format the host actually speaks. It takes
  `status_code`, `status_message`, `children` (as `[name, text]` pairs), and
  `present` (names only). The body is parsed with the same
  [`gs_proto::xml::Element`](../../crates/gs-proto/src/xml.rs) model the daemon
  uses, so field order and whitespace do not matter.
- `content_type` is optional; captured responses are `application/xml`.
- `json` is a **subset** match, retained for endpoints that turn out to be JSON.
- `body` is an exact byte match, for responses the protocol fixes completely.
- `certificate_der_hex` is the client certificate in DER form, hex-encoded, for
  fixtures that exercise a paired route.

A real capture lives in [`http/oracle-unpaired.jsonl`](http/oracle-unpaired.jsonl);
its provenance and the findings it forced are in [`m0/README.md`](m0/README.md).

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

## RTSP fixture format (M2-T9)

The `rtsp/` directory is consumed by the replay harness in
[`crates/gs-rtsp/src/replay.rs`](../../crates/gs-rtsp/src/replay.rs) and the
integration test [`crates/gs-rtsp/tests/golden_traces.rs`](../../crates/gs-rtsp/tests/golden_traces.rs).
Each `*.jsonl` file holds one capture; every non-empty, non-`#` line is one JSON
object. A fixture is one or two captured wire messages plus the fields the
protocol fixes for them.

```json
{
  "name": "describe-request",
  "request_wire": "DESCRIBE rtsp://192.168.1.2:48010 RTSP/1.0\r\nCSeq: 2\r\nAccept: application/sdp\r\n\r\n",
  "expect_request": {
    "method": "DESCRIBE",
    "uri": "rtsp://192.168.1.2:48010",
    "cseq": 2,
    "headers": [["Accept", "application/sdp"]],
    "body": ""
  }
}
```

Rules for the fixture:

- At least one of `request_wire`/`response_wire` must be present; a fixture with
  neither is refused rather than counted as a pass.
- `*_wire` is the **exact captured bytes**, with `\r\n` JSON-escaped. The replay
  asserts the parser consumes exactly those bytes and no more (framing check).
- `expect_request` / `expect_response` fields are all optional and are asserted
  individually: `method`/`status`/`reason`/`uri`, numeric `cseq`, header pairs,
  and `body`.
- `rtspenc_key_hex` is the 16-byte `rikey` the host sent on `/launch`, for a
  capture that used encrypted RTSP. When present, each wire message is also
  round-tripped through [`gs_rtsp::RtspCipher`](../../crates/gs-rtsp/src/encrypt.rs),
  asserting the `ciphertext || tag` framing and that decryption reproduces the
  capture. Both directions start at sequence zero, so a multi-message encrypted
  capture should be split into one fixture per message.

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
