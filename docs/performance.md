# Performance and latency

This is the tracked record of the latency work behind **0.1.1**. It separates
**objective, automated measurements** from **subjective glass-to-glass
observations**, because the receiver does not expose a reliable live-edge
position and the final number is ultimately what a human sees.

Full raw spike logs and helper scripts are kept locally under the gitignored
`plans/` directory; this document is the reproducible summary.

## Environment

| Item | Value |
|---|---|
| Receiver | Panasonic TV, `PANASONIC TV`, build `3.72.446070` |
| Receiver IP | `192.168.100.54` (`eureka_info`: `ethernet_connected: true`) |
| Host | Lenovo laptop, `enp1s0` wired `192.168.100.45/24`, `wlp2s0` DOWN |
| Capture output | `HEADLESS-spike`, 1920x1080@60 |
| Codec / pipeline | H.264 VAAPI → fMP4 → LL-HLS tags → in-memory HTTP server |
| Date | 2026-10-30 |

> **Tailscale caveat.** A policy route for `192.168.100.0/24` via `tailscale0`
> was active during the first attempt. It was stopped before measuring so the
> numbers below describe the direct wired LAN, not the tailnet.

## Objective measurements

### Network leg (direct wired path)

```text
20 packets transmitted, 20 received, 0% packet loss
rtt min/avg/max/mdev = 0.298 / 0.368 / 0.422 / 0.037 ms
```

- Cast control port `8009/tcp` reachable; `8008` (`eureka_info`) reachable.
- UFW must allow `5353/udp` (mDNS) and the stream port (`7236/tcp` by default).
  Without those rules the receiver's discovery packets and media fetches are
  silently dropped (UFW's block logging is rate-limited, so the TCP drop can be
  invisible in the log).
- The receiver fetched three ~611 KB segments within **~0.3 s** of the `LOAD`
  (~1.8 MB burst, ~49 Mbps), then one every **~2.1 s** (~2.3 Mbps sustained).
  Maximum segment-fetch gap was **2.23 s**, with no retries or stalls.

### Time-to-first-frame: LL-HLS vs plain fMP4

Same packager, same segments, same 30 s window, identical wired link; only the
playlist metadata differs.

| Playlist | TTFF (`LOAD` → first `PLAYING`) | Full segments fetched | Fetch cadence |
|---|---|---|---|
| LL-HLS tags (`PART-HOLD-BACK=1.5`, `#EXT-X-PART`, preload hint) | **4–7 s** (7 s in the 30 s run) | 17 | 2.1–2.2 s |
| Plain fMP4 (no LL-HLS tags) | **~16 s** | 17 | 2.1–2.2 s |

The network is fast and identical in both runs, yet the plain playlist takes
more than twice as long to start. That is **receiver-side HLS holding**, not
Wi-Fi.

### Startup path (0.1.1 fixes)

| Step | Before 0.1.1 | After 0.1.1 |
|---|---|---|
| Manual `--host` lookup | fixed 10 s mDNS browse | direct connect first |
| `init segment ready` → `casting` | ~10.9 s | **~0.5 s** |
| Native app holding the session | `LOAD` accepted but never fetched | Default Media Receiver launched (`CC1AD845`), first playlist request ~0.1 s later |
| Receiver still `IDLE` during load | aborted with “did not start playback” | waits for `PLAYING`/`BUFFERING` |

## Subjective glass-to-glass observations

The receiver's `current_time` is **not** a reliable latency proxy here: it
advances in ~3 s steps and alternates `PLAYING`/`BUFFERING`, so the old
`current_time`-based estimate cannot be used.

Method used for the subjective numbers:

1. Show a running clock on the laptop output and cast that output to the TV.
2. Compare the two clocks from a single viewing position (or a photo of both).
3. Report the delta; the result is **coarse (±0.5 s)** and single-observer.

| Spike | Link | Playlist | User-observed glass-to-glass | Notes |
|---|---|---|---|---|
| H | Wi-Fi | HLS, 1 s segments, list 4 | **~3–5 s** | stable |
| H | Wi-Fi | HLS, 0.5 s segments, list 3 | **>5 s** | receiver crashed to the Screencast logo |
| J | Wi-Fi | Plain fMP4 HLS | **>4 s** | no LL-HLS tags |
| J | Wi-Fi | Hybrid LL-HLS | **<2 s** | confirmed across two runs; receiver still fetched 0 parts |
| M | Wired | LL-HLS | **not freshly observed** | objective run only; expected to remain at the ~2 s receiver floor |

The wired re-measurement did not repeat the clock comparison; it measured the
network and startup objectively. Because the wired RTT is 0.37 ms with 0.04 ms
jitter, and the receiver fetches the media in a 0.3 s burst, there is no
plausible way for Wi-Fi-class latency to account for the multi-second delay.

## Interpretation

- **Wi-Fi was not the dominant cause.** Wired Ethernet removes jitter and
  retransmit risk, but the receiver's HLS buffering/startup policy still
  dominates.
- **LL-HLS tags remain the main latency lever.** Even though this Panasonic
  ignores `#EXT-X-PART` (it fetches full segments only), the LL-HLS metadata
  roughly halves startup latency versus a plain fMP4 playlist.
- **0.1.1 removes avoidable delays**, not the receiver floor: host-first
  connect, forced receiver takeover, and a correct playback wait. Sub-second
  latency is still not a goal; the honest floor is **~2 s** on this receiver.

## Reproduce

```sh
# network leg
ping -c 20 192.168.100.54
nc -z -w 3 192.168.100.54 8009

# LL-HLS vs plain fMP4 (local spike helper; see plans/spikes/)
python3 plans/spikes/cast_probe.py 192.168.100.54 30 ll
python3 plans/spikes/cast_probe.py 192.168.100.54 30 plain

# real engine path (host-first connect, takeover, playback wait)
python3 scripts/cast_engine.py stream \
  --output HEADLESS-spike --host 192.168.100.54 --duration 20
```

Ensure the headless output exists (`hyprctl output create headless
HEADLESS-spike`) and that the stream port is allowed through the firewall before
running the engine command.
