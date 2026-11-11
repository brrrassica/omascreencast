# M0 runbook — spikes and the oracle

This runbook covers the M0 tasks in
[`plans/towards-0.2.0.md`](../plans/towards-0.2.0.md) that need hardware that
cannot be scripted from CI: a physical Android TV running Moonlight, a LAN, and
a real Sunshine checkout used as a black-box oracle.

Everything that **can** be automated is already implemented:

| Task | Status | Evidence |
|---|---|---|
| M0-T1 — local oracle | **mostly done** | a built Sunshine is on the box (`/usr/libexec/monitorize/sunshine`, commit `8d043f2b`) and Moonlight 6.2.0 (`moonlight-qt`, same `moonlight-common-c` stack) pairs with it. The plan asks specifically for **Moonlight for Android TV** — that leg still needs the TV reachable. Oracle commit recorded in the fixtures provenance. |
| M0-T2 — fake server | **automated** | [`tools/m0-gamestream/fake_server.py`](../tools/m0-gamestream/fake_server.py), tested by [`tests/test_m0_spike.py`](../tests/test_m0_spike.py). Upgraded 2026-11-11 to the capture-verified wire shape: dual ports (plaintext discovery 47989 serving only `/serverinfo`, TLS control 47984), XML `<root status_code=...>` envelopes, and the **presented client certificate recorded** in every TLS log entry. |
| M0-T3 — FEC 0 | **partial pass** | Moonlight 6.2.0 completes the full pairing exchange against the oracle with `fec_percentage = 0`. Session-level "stays up" not reached without a rendering client. Recorded in [`fixtures/m0/README.md`](../omascreencastd/tests/fixtures/m0/README.md). |
| M0-T5 — discovery exchange | **partial** | First client-driven capture frozen: [`fixtures/http/moonlight-discovery.jsonl`](../omascreencastd/tests/fixtures/http/moonlight-discovery.jsonl), replayed by the M1-T11 golden-trace test. Session traces (`/launch`, RTSP, control, RTP) still missing. |
| M0-T6 — clean-room wording | **done** | [`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md), [`CONTRIBUTING.md`](../CONTRIBUTING.md) |
| M0-T7 — trace corpus | **partially frozen** | [`omascreencastd/tests/fixtures/README.md`](../omascreencastd/tests/fixtures/README.md) plus the two `http/` captures with provenance rows. |

## The no-TV recipe (discovered 2026-11-11)

`moonlight-qt` on the host speaks the same `moonlight-common-c` protocol as
Moonlight for Android TV, and its CLI can complete the whole pairing handshake
without a TV — this is how the first client-driven captures were taken:

```sh
# 1. Start the oracle reading its PIN from a fifo:
cd /tmp && mkfifo pin.fifo
(sleep 600 > pin.fifo) &            # feeder blocks until sunshine opens it
/usr/libexec/monitorize/sunshine <conf> -0 < pin.fifo > sunshine.log 2>&1 &

# 2. Start the capture (root), then pair with the PIN the feeder holds:
nohup timeout 300 tshark -i lo -s0 -w /tmp/m0.pcap \
  'tcp port 47984 or tcp port 47989 or tcp port 48010 or udp' &
moonlight pair <host-ip> --pin 9662

# 3. Extract: -Y http.request / -Y http.response on 47989 gives the
#    discovery exchange verbatim; 47984 payloads are opaque (the client does
#    not write an SSLKEYLOGFILE), so pairing vectors need the fake server —
#    its TLS socket has CERT_OPTIONAL and logs the client certificate + bodies.
```

The `pairing/`, `rtsp/`, `control/`, and `video/` corpora still need a client
that can render a session (the GUI `stream` path on a desktop session, or the
TV itself); the headless CLI stream attempt does not reach `PLAY` here.

The tasks below are **manual**. Run them in order on the machine that will host
the stream, then record each answer in the trace corpus and tick the box in the
plan.

> Constraint respected throughout: the oracle is a **black box**. Run Sunshine,
> watch its network behaviour, and copy bytes off the wire. Do **not** read its
> source to write ours (D1, [`CONTRIBUTING.md`](../CONTRIBUTING.md)).

---

## Prerequisites

- The Android TV and the host on the **same wired LAN** where possible.
- Moonlight for Android TV installed and able to reach the host.
- Host tooling: `git`, `cargo`/build deps for Sunshine, `ffmpeg`,
  `python3` with `zeroconf`, `openssl`, and `tcpdump` (or Wireshark).
- Ports used below: plaintext discovery `47989`, HTTPS `47984`, RTSP `48010`.
  Confirm they are free:

  ```sh
  ss -lntup | grep -E '47989|47984|48010' && echo 'in use' || echo 'free'
  ```

  A capture on this machine settled what each port does:

  | Port | Scheme | Answers | Trust |
  |---|---|---|---|
  | 47989 | HTTP | `/serverinfo` only; every other path is `<root status_code="404"/>` | none |
  | 47984 | HTTPS | pairing, app list, launch | **requires a trusted client certificate on every route** |
  | 48010 | RTSP | `DESCRIBE`/`SETUP`/`PLAY` | per pairing |

  Discovery is XML on 47989 (`curl -s http://<host>:47989/serverinfo`), and a
  protocol failure is HTTP 200 with `status_code` in the body — not the HTTP
  status.

---

## M0-T1 — Stand up the local oracle

Goal: prove Moonlight for Android TV can connect to a known-good GameStream
host, and capture the exchange. This oracle is **never shipped**.

```sh
# from the repo root; upstream-sunshine/ is gitignored already
git clone https://github.com/LizardByte/Sunshine.git upstream-sunshine
cd upstream-sunshine
# build per its own instructions for your distro; keep it entirely inside this dir
# e.g. (adjust to what the checkout documents — do not copy code, just commands)
cmake -S . -B build -DCMAKE_BUILD_TYPE=Release
cmake --build build -j"$(nproc)"
```

Run it and confirm it advertises:

```sh
# run the built sunshine binary; it will prompt for a PIN setup on first run
./build/sunshine &
# verify mDNS advertisement is visible
avahi-browse -rtp _nvstream._tcp
```

On the TV:

1. Open Moonlight; the host should appear (name comes from the oracle's hostname).
2. Select it, note the 4-digit PIN it shows, and enter it in Sunshine's web UI
   (`https://<host>:47990`) to pair.
3. Confirm the app list appears. **Record the Moonlight app + version.**

**Record:** Moonlight version, Sunshine commit (`git -C upstream-sunshine rev-parse HEAD`),
and that pairing + app list succeeded. This is the baseline the fake server and
the daemon must match.

---

## M0-T2 — Fake server (already automated)

Smoke-check the responder before using it for the FEC and audio spikes:

```sh
python3 tools/m0-gamestream/fake_server.py \
  --log omascreencastd/tests/fixtures/m0/spike-capture.jsonl \
  --hostname omascreencast-m0-spike
```

It prints the bound URL and advertises `_nvstream._tcp`. From another shell,
sanity-check the routes over TLS (the spike cert is self-signed):

```sh
# Discovery is plaintext and XML; no certificate is needed.
curl -s http://<host-ip>:47989/serverinfo
# Everything else needs a *trusted* certificate on 47984. Without one the
# handshake is refused outright; with an untrusted one every route answers a
# 401 envelope. Both are expected until pairing completes.
curl -sk https://<host-ip>:47984/serverinfo
```

Every request lands in `spike-capture.jsonl` verbatim (method line, headers,
query, body). Stop it with `Ctrl-C` (or `--seconds N`).

---

## M0-T3 — Verify `fecPercentage=0` (D5)

Question: does Moonlight for Android TV connect and stay up when the server
advertises **FEC 0** and does not implement Reed–Solomon?

The fake server advertises no FEC. So the check is simply: point Moonlight at
the fake server and see whether it connects and survives.

1. Start the fake server (M0-T2 command above).
2. On the TV, open Moonlight and select `omascreencast-m0-spike`.
3. If it prompts for a PIN, note what it sends — the spike logs the request but
   does **not** complete pairing. That limitation is expected; record it.
4. Observe for ~5 minutes.

**Record one of:**

- **Pass:** Moonlight connects (or reaches the pairing prompt) without a
  FEC-related error, and does not disconnect/retry in a loop. → D5 stands.
- **Fail:** record the exact on-screen error, the app version, and the
  `spike-capture.jsonl` tail. → **A negative result is a flagged finding, not a
  silent scope change** (plan §3). Re-open D5 in the plan before writing code.

Because the spike does not pair, if Moonlight refuses to proceed past pairing,
record that as **"blocked by pairing"** rather than a FEC verdict, and re-run
T3 after M1-T4 lands or by pointing at the oracle with FEC forced off.

---

## M0-T4 — Verify video-only tolerance (D6)

Question: does Moonlight play a stream whose SDP has **no audio stream**?

The fake server can serve an SDP with no audio m-line:

```sh
python3 tools/m0-gamestream/fake_server.py --no-audio \
  --log omascreencastd/tests/fixtures/m0/spike-capture.jsonl
```

Also useful as the reference for what an audio-less SDP looks like:

```sh
curl -sk https://<host-ip>:47984/sdp
# expect: m=video present, no m=audio line
```

1. Run the fake server with `--no-audio`.
2. Connect Moonlight as in T3.
3. Observe whether it plays video (or reaches the point where it would) without
   erroring or retrying on the missing audio stream.

**Record:** pass/fail, the exact behaviour, and whether Moonlight requested audio
anyway (check the `raw_query` on `/launch` for `localAudio` / `surroundAudioInfo`).

A negative result flags D6 for re-opening; it does not silently drop audio from
the plan.

---

## M0-T5 — Capture the full exchange

Capture what the client actually sends, to freeze the trace corpus (M0-T7).

### From the fake server

The fake server already writes JSONL. Run it with the corpus path:

```sh
python3 tools/m0-gamestream/fake_server.py \
  --log omascreencastd/tests/fixtures/m0/spike-capture.jsonl
```

Drive it through the TV as in T3/T4, then copy the relevant lines into the
topic directories (`http/`, `rtsp/`, `control/`, `video/`) as described in
[`omascreencastd/tests/fixtures/README.md`](../omascreencastd/tests/fixtures/README.md).

### From the oracle, over the wire

The fake server does not implement RTSP or RTP, so those traces must come from
the oracle (M0-T1). Capture the wire without reading source:

```sh
# HTTPS + RTSP setup, and the start of UDP media
sudo tcpdump -i any -s 0 -w /tmp/m0-oracle.pcap \
  'host <tv-ip> and (tcp port 47984 or tcp port 48010 or udp)'
```

Then, on the TV, launch a stream from the oracle and let it run ~10 seconds.
Stop `tcpdump`. Extract the pieces you need:

- `/launch` and `/resume` **query parameters** → `http/`
- RTSP `DESCRIBE` / `SETUP` / `PLAY` framing → `rtsp/`
- control-channel handshake (first packets on the control port) → `control/`
- first ~20 video RTP packets after `PLAY` → `video/`

Redact keys, PINs, MACs, and IPs (see the fixtures README), and fill in the
provenance table in [`omascreencastd/tests/fixtures/m0/README.md`](../omascreencastd/tests/fixtures/m0/README.md).

### Freeze

Commit the captured files plus their provenance. Tick **M0-T5 and M0-T7** in the
plan only once the fixtures exist on disk. If a capture is incomplete because a
spike was blocked, record that as a finding rather than filling the gap by hand.

---

## Exit criteria

M0 is done when:

- [x] M0-T1 oracle connects and pairs (recorded; desktop Moonlight 6.2.0 —
      Android TV leg pending the physical TV).
- [x] M0-T3 `fecPercentage=0` has a recorded pass up to pairing (session leg
      pending a rendering client); no negative signal (D5 stands for now).
- [ ] M0-T4 video-only has a recorded pass/fail.
- [ ] M0-T5 exchanges are captured.
- [ ] M0-T7 corpus is committed with provenance and a regeneration note.

Any **negative** answer is escalated in the plan, not worked around later.
