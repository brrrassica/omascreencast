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
