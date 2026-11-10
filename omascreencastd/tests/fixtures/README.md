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
