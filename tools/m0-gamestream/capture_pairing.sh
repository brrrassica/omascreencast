#!/usr/bin/env bash
# M0-T5 capture: start the local Sunshine oracle as a black box, pair a real
# Moonlight client against it, and write the decrypted exchange to disk.
#
# Spike tooling, not product code (D1: the oracle is a black box; no source is
# read). Decryption comes from tools/m0-gamestream/sslkeylog.c, which asks
# OpenSSL for the session secrets on both sides, so `tshark` can read loopback.
#
# Usage: sudo tools/m0-gamestream/capture_pairing.sh <work-dir> <pin>
set -euo pipefail

WORK="${1:?work directory}"
PIN="${2:?four-digit pin}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ORACLE="${ORACLE:-/usr/libexec/monitorize/sunshine}"
SHIM="$WORK/sslkeylog.so"

mkdir -p "$WORK"
cd "$WORK"
rm -f keys.log capture.pcap sunshine.log moonlight.log tshark.log http.txt
printf '' > conf
rm -f pin.fifo; mkfifo pin.fifo

echo "== building keylog shim"
gcc -shared -fPIC -O2 -o "$SHIM" "$REPO_ROOT/tools/m0-gamestream/sslkeylog.c" -ldl

echo "== starting capture"
# `-i any` rather than `lo`: a client given a LAN address routes to the LAN
# interface, so a loopback-only capture sees the TLS handshake and nothing else.
tshark -i any -s 0 -w capture.pcap -q \
  'tcp port 47984 or tcp port 47989 or tcp port 48010 or udp portrange 47998-48010' \
  > tshark.log 2>&1 &
TSHARK_PID=$!
sleep 2

echo "== starting oracle"
# -1: pristine state (never paired), -0: read the PIN from stdin.
( sleep 120; echo "$PIN" ) > pin.fifo &
FEED_PID=$!
SSLKEYLOGFILE="$WORK/keys.log" LD_PRELOAD="$SHIM" \
  "$ORACLE" "$WORK/conf" -1 -0 sunshine_name=m0-oracle \
    address=127.0.0.1 port=47984 upnp=disabled fec_percentage=0 \
  < pin.fifo > sunshine.log 2>&1 &
SUN_PID=$!

for _ in $(seq 1 20); do
  ss -ltn | grep -q ':47984 ' && break
  sleep 0.5
done
ss -ltn | grep -q ':47984 ' || { echo "oracle did not start" >&2; exit 3; }
echo "== oracle listening"

echo "== pairing"
set +e
SSLKEYLOGFILE="$WORK/keys.log" LD_PRELOAD="$SHIM" \
  timeout 90 script -qec "moonlight pair 127.0.0.1 --pin $PIN" /dev/null \
  > moonlight.log 2>&1
PAIR_RC=$?
set -e
echo "pair exit=$PAIR_RC"

sleep 2
kill "$SUN_PID" "$FEED_PID" 2>/dev/null || true
sleep 1
kill "$TSHARK_PID" 2>/dev/null || true
wait "$TSHARK_PID" 2>/dev/null || true

echo "== keylog lines: $(wc -l < keys.log 2>/dev/null || echo 0)"
echo "== capture bytes: $(stat -c %s capture.pcap 2>/dev/null || echo 0)"

echo "== decoding"
tshark -r capture.pcap -o "tls.keylog_file:$WORK/keys.log" \
  -Y 'http.request or http.response or rtsp' -T fields \
  -e frame.number -e ip.src -e ip.dst -e tcp.srcport -e tcp.dstport \
  -e http.request.method -e http.request.uri -e http.response.code -e rtsp.method \
  > http.txt 2>/dev/null || true
echo "== decoded rows: $(wc -l < http.txt)"
exit "$PAIR_RC"
