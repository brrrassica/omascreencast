# M0 provenance log

One row per captured file in this directory. See [`../README.md`](../README.md)
for the field meanings.

| File | captured_at | client | server | task | method | tool | redacted |
|---|---|---|---|---|---|---|---|
| [`../http/oracle-unpaired.jsonl`](../http/oracle-unpaired.jsonl) | 2026-11-11T02:44Z | `curl` (stand-in; Moonlight capture pending) | `sunshine` (local `monitorize/sunshine-1` service) | M0-T5 (partial) | `curl` against the live oracle | `curl -s http://127.0.0.1:47989/serverinfo` on port 47989 | none (discovery exposes no key or PIN) |
| [`../http/moonlight-discovery.jsonl`](../http/moonlight-discovery.jsonl) | 2026-11-11T03:54Z | Moonlight 6.2.0 (`moonlight-qt` CLI; same `moonlight-common-c` protocol stack as Moonlight for Android TV) | `sunshine` 0.0.0 commit `8d043f2b` (fresh local oracle, never paired) | M0-T5 (partial) | packet capture of plaintext discovery on loopback | `tshark -i lo -s0 -w <pcap> 'tcp port 47989 or ...'`, then `tshark -Y http.request` / `-Y http.response` field extraction | none (discovery exposes no key or PIN) |
| [`moonlight-pair-request.jsonl`](moonlight-pair-request.jsonl) | 2026-11-12T12:45Z | Moonlight 6.2.0 (`moonlight-qt` CLI) | `omascreencastd` (this repo) | M0-T5 (pairing request shape) | the daemon's own request log; it terminates TLS so it sees the plaintext request | `moonlight pair <host> --pin <pin>` against a local `omascreencastd serve`, `RUST_LOG=debug` | client certificate DER truncated (present, not reproduced) |

## What the captures proved (and why they changed the daemon)

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
   **Superseded 2026-11-12 by finding 11 below.** This was a `curl` probe, not a
   client pairing; a real client does start pairing on 47989.
6. **`ExternalPort` echoes the serving port**, not `HttpsPort` (47989 vs 47984).
   The daemon reports the HTTPS port instead; the divergence is asserted nowhere.
7. **`currentgame` is a 32-bit integer** on the wire; the DTO was widened from
   `u16`.

Findings from the Moonlight-driven capture (`moonlight-discovery.jsonl`),
2026-11-11:

8. **`PairStatus` means "clients paired > 0", not "unpaired".** The fresh
   never-paired oracle reported `PairStatus: 0`; the monitorize host above
   (which has a paired client) reported `PairStatus: 1`. The earlier reading
   ("1 == unpaired") was wrong and is corrected in `gs_proto` constants. The
   replay harness gained a `paired` field in its `server` directive so each
   capture's host state is reproduced faithfully.
9. **Moonlight polls discovery with a fresh `uuid` query value per request.**
   The captured client sent `/serverinfo?uniqueid=<its-id>&uuid=<random>`
   repeatedly (every ~3 s while the UI waits). `uniqueid` is the *client's*
   identity, not the host's — the earlier fixture asserted the wrong thing
   about the query; the capture fixes it.
10. **`GfeVersion` is not pinned by Moonlight 6.2.0's discovery.** The oracle
    reports `3.23.0.74`, this daemon reports `7.1.431.0`, and the client
    accepted both ("Latest supported GFE server: 99.99.99.99" either way).
    Which value a real Android TV build requires remains an open spike on the
    TV itself.

Findings from the pairing-start capture (`moonlight-pair-request.jsonl`),
2026-11-12 — these **overturn four M1 assumptions**:

11. **Pairing does *not* start on the trusted HTTPS port.** The client's first
    pairing call is `GET /pair` on the **plaintext discovery listener (47989)**.
    Finding 5 above ("only `/serverinfo` is served on 47989 … pairing cannot be
    started over the unauthenticated socket") was inferred from a `curl` probe
    and had never been exercised by a client that was actually pairing. It is
    corrected here: the discovery listener now serves the pairing start,
    gated by `Route::allows_unpaired`, so no session route is reachable there.
12. **`/pair` is a phrase dispatcher, not one step.** The whole handshake is
    driven by `phrase=` values on this single path. The separate
    `/clientchallenge`, `/serverchallengeresp`, and `/clientpairingsecret`
    routes the M1 design assumed are still served for compatibility, but a real
    client never calls them.
13. **The salt is chosen by the client and sent on the first request**
    (`salt=<32 hex chars>`). The host minting the salt — the M1 design — means
    the two sides can never agree on key material, so the KDF could not have
    worked no matter how correct its bytes were. Salt ownership is now a
    parameter (`PairingManager::begin_with_salt`).
14. **The client certificate arrives in the query string** (`clientcert=<hex DER>`)
    on the first request, alongside a TLS client certificate later. Trust
    anchoring must therefore accept the query parameter as an input, not only
    the TLS peer certificate.

Also confirmed against the local oracle: **HTTPS `/pair` without a usable
client certificate answers a self-closing `<root status_code="404"/>`**, which
matches the XML envelope and shows the pairing route is certificate-gated on the
trusted port.

## M0-T3 — FEC 0 with a real client (2026-11-11, partial)

The local oracle was rebuilt-in-config with `fec_percentage = 0` and
`moonlight-qt 6.2.0` **completed the full pairing exchange against it**
(`moonlight pair --pin <PIN>` exited 0). No FEC-related error was observed at
discovery or pairing. **What was not reached:** a sustained stream session —
the headless CLI stream attempt did not produce a playable session on this
machine (windowed SDL on a headless compositor), so "connects and stays up"
is only confirmed up to pairing, not to `PLAY`. **Verdict: provisional pass
for D5** — no negative signal anywhere; the remaining half of the question
needs the TV (or a machine where the Moonlight GUI renders).

## Still needed to close M0

The clock capture above is a manual `curl` stand-in, not a Moonlight session, so
it does **not** close M0-T4 (video-only), or M0-T5 in full. Those need the
Android TV pointed at the host with a recording proxy, per
[`../../docs/m0-runbook.md`](../../docs/m0-runbook.md). The
`/applist` and `/launch` XML shapes remain **provisional** until a trusted
client certificate lets a capture reach them.

**Update 2026-11-11:** pairing *is* reachable with `moonlight-qt` — it can
pair with `--pin` when the host reads the PIN from stdin, so a session
capture (`/launch`, RTSP, control, first RTP packets) no longer strictly
requires the TV; it requires a host where the Moonlight GUI can render the
session window. Pairing vectors can therefore be produced without inventing
any bytes.

## Notes

- `spike-capture.jsonl` is the default `--log` target of
  [`tools/m0-gamestream/fake_server.py`](../../../../tools/m0-gamestream/fake_server.py).
- `oracle-*.jsonl` are captures from the real Sunshine oracle used as a black
  box (M0-T1). Record the oracle commit in `server`; never copy its source.
- Every capture must correspond to a step in
  [`docs/m0-runbook.md`](../../../../docs/m0-runbook.md).
