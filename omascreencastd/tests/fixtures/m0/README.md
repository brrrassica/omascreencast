# M0 provenance log

One row per captured file in this directory. See [`../README.md`](../README.md)
for the field meanings.

| File | captured_at | client | server | task | method | tool | redacted |
|---|---|---|---|---|---|---|---|
| [`../http/oracle-unpaired.jsonl`](../http/oracle-unpaired.jsonl) | 2026-11-11T02:44Z | `curl` (stand-in; Moonlight capture pending) | `sunshine` (local `monitorize/sunshine-1` service) | M0-T5 (partial) | `curl` against the live oracle | `curl -s http://127.0.0.1:47989/serverinfo` on port 47989 | none (discovery exposes no key or PIN) |

## What this capture proved (and why it changed the daemon)

These are findings, not assumptions. Each one corrected a guess made before the
capture existed:

1. **Discovery is plaintext on 47989, not TLS.** `/serverinfo` answers with no
   client certificate on plaintext HTTP port 47989. Over HTTPS 47984 the oracle
   answered a self-signed client certificate with `401 The client is not
   authorized. Certificate verification failed.`, and with *no* certificate it
   dropped the TLS handshake entirely (`tlsv13 alert certificate required`).
2. **The control endpoints speak XML, not JSON.** This corrects the M1 JSON
   assumption that came from the M0 fake responder. Bodies are
   `<?xml version="1.0" encoding="utf-8"?>` followed by
   `<root status_code="…">…</root>`.
3. **A protocol failure is HTTP `200`.** The real status is the `status_code`
   attribute inside the body. `/launch` unpaired is HTTP 200 with
   `status_code="401"`; an unknown path is HTTP 200 with `status_code="404"`.
4. **An unknown path is a bare self-closing envelope**: `<root status_code="404"/>`
   with no `status_message`.
5. **Only `/serverinfo` is served on 47989.** `/pair`, `/applist`, `/launch`, and
   `/` all return the same `404` envelope, so pairing cannot be started over the
   unauthenticated socket.
6. **`ExternalPort` echoes the serving port**, not `HttpsPort` (47989 vs 47984).
   The daemon reports the HTTPS port instead; the divergence is asserted nowhere.
7. **`currentgame` is a 32-bit integer** on the wire; the DTO was widened from
   `u16`.

## Still needed to close M0

The clock capture above is a manual `curl` stand-in, not a Moonlight session, so
it does **not** close M0-T3 (FEC), M0-T4 (video-only), or M0-T5 in full. Those
need the Android TV pointed at the host with `tcpdump` (or the fake server)
recording, per [`../../docs/m0-runbook.md`](../../docs/m0-runbook.md). The
`/applist` and `/launch` XML shapes remain **provisional** until a trusted
client certificate lets a capture reach them.

## Notes

- `spike-capture.jsonl` is the default `--log` target of
  [`tools/m0-gamestream/fake_server.py`](../../../../tools/m0-gamestream/fake_server.py).
- `oracle-*.jsonl` are captures from the real Sunshine oracle used as a black
  box (M0-T1). Record the oracle commit in `server`; never copy its source.
- Every capture must correspond to a step in
  [`docs/m0-runbook.md`](../../../../docs/m0-runbook.md).
