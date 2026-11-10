#!/usr/bin/env python3
"""Throwaway GameStream HTTPS responder for milestone M0.

This is **spike tooling, not product code**. It exists to answer the M0
questions in ``plans/towards-0.2.0.md``:

* M0-T2 — advertise ``_nvstream._tcp``, answer ``/serverinfo`` and
  ``/applist``, and log every request verbatim.
* M0-T5 — capture ``/launch`` and ``/resume`` query parameters (and any other
  request Moonlight makes) as JSONL for the frozen trace corpus.

It deliberately does **not** stream video and does **not** implement pairing
cryptography. Its only job is to be reachable, plausible enough for Moonlight
to talk to, and honest about what the client sent.

Design rules
------------
* Standard library only for the HTTP/TLS path, so it runs on a clean Arch box
  with no venv. ``zeroconf`` is imported lazily and only when advertising.
* Every request is recorded before a response is chosen, including unknown
  paths, so a 404 still lands in the capture.
* Responses come from :class:`ServerConfig` and can be stubbed per run. The
  field names below follow what Moonlight reads, but the *values* are
  placeholders: treat the oracle (M0-T1) capture as the source of truth.

Usage
-----
    # capture to the default trace path, advertise over mDNS
    python3 tools/m0-gamestream/fake_server.py --log capture.jsonl

    # plain HTTP, no mDNS, ephemeral port (used by the unit tests)
    python3 tools/m0-gamestream/fake_server.py --no-tls --no-advertise --port 0

    # emit an SDP without an audio stream for the M0-T4 check
    python3 tools/m0-gamestream/fake_server.py --no-audio
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import signal
import ssl
import subprocess
import sys
import threading
import time
import uuid as uuidlib
from dataclasses import dataclass, field
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs, urlparse

# Placeholder identities. The real oracle capture replaces these; they only
# need to be internally consistent for Moonlight to accept the server.
SERVER_NAME = "omascreencast-m0-spike"
APP_VERSION = "7.1.431.-1"
GFE_VERSION = "7.1.431.0"
SERVER_CODEC_MODE_SUPPORT = 259  # H.264 | HEVC | AV1 advertising mask used by Sunshine
DEFAULT_HTTPS_PORT = 47984
DEFAULT_RTSP_PORT = 48010

#: Millisecond-ish pause to let a client finish reading before shutdown.
SHUTDOWN_GRACE_SECONDS = 0.1


@dataclass
class ServerConfig:
    """Everything a response needs, plus what gets echoed into mDNS TXT."""

    bind: str = "0.0.0.0"
    port: int = DEFAULT_HTTPS_PORT
    rtsp_port: int = DEFAULT_RTSP_PORT
    hostname: str = SERVER_NAME
    server_uuid: str = field(default_factory=lambda: str(uuidlib.uuid4()))
    pair_status: int = 1  # 1 == unpaired, so Moonlight exercises pairing
    no_audio: bool = False
    log_path: Path | None = None
    local_ip: str | None = None

    def resolved_local_ip(self) -> str:
        if self.local_ip:
            return self.local_ip
        # Best-effort: a UDP connect picks the default-route address without
        # sending a packet. Falls back to loopback when offline.
        import socket

        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            sock.connect(("192.0.2.1", 9))  # TEST-NET-1, never actually reached
            return sock.getsockname()[0]
        except OSError:
            return "127.0.0.1"
        finally:
            sock.close()


class RequestLog:
    """Append-only JSONL log of every request, ordered and thread-safe."""

    def __init__(self, path: Path | None) -> None:
        self.path = path
        self._lock = threading.Lock()
        self._fh = None
        if path is not None:
            path.parent.mkdir(parents=True, exist_ok=True)
            self._fh = path.open("a", encoding="utf-8")

    def record(self, entry: dict[str, Any]) -> None:
        entry.setdefault("received_at", datetime.now(timezone.utc).isoformat())
        with self._lock:
            if self._fh is not None:
                self._fh.write(json.dumps(entry, ensure_ascii=False) + "\n")
                self._fh.flush()
            else:
                print(json.dumps(entry, ensure_ascii=False), file=sys.stderr)

    def close(self) -> None:
        with self._lock:
            if self._fh is not None:
                self._fh.close()
                self._fh = None


def _body_to_json(body: bytes, content_type: str) -> dict[str, Any] | str | None:
    if not body:
        return None
    if "json" in content_type.lower():
        try:
            return json.loads(body.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            pass
    text = body.decode("utf-8", errors="replace")
    if any(ord(ch) < 9 or (13 < ord(ch) < 32) for ch in text):
        return {"base64": base64.b64encode(body).decode("ascii")}
    return text


def serverinfo(config: ServerConfig) -> dict[str, Any]:
    ip = config.resolved_local_ip()
    return {
        "hostname": config.hostname,
        "appversion": APP_VERSION,
        "GfeVersion": GFE_VERSION,
        "uniqueid": config.server_uuid,
        "HttpsPort": config.port,
        "ExternalPort": config.port,
        "mac": "00:11:22:33:44:55",
        "LocalIP": ip,
        "ServerCodecModeSupport": SERVER_CODEC_MODE_SUPPORT,
        "PairStatus": config.pair_status,
        "currentgame": 0,
        "state": "SUNSHINE_SERVER_FREE",
        "SessionUrl0": f"rtsp://{ip}:{config.rtsp_port}",
    }


def applist(config: ServerConfig) -> dict[str, Any]:
    # A single synthetic app is all omascreencast needs (M2-T5).
    return {
        "AppState": "SUNSHINE_SERVER_FREE",
        "App": [
            {
                "AppTitle": "Desktop",
                "ID": "1",
                "IsHdrSupported": "0",
                "HdrSupported": False,
            }
        ],
    }


def launch_response(config: ServerConfig) -> dict[str, Any]:
    ip = config.resolved_local_ip()
    return {
        "Gamesession": "1",
        "SessionUrl0": f"rtsp://{ip}:{config.rtsp_port}",
    }


def build_sdp(config: ServerConfig, host: str) -> str:
    """Minimal H.264 SDP. ``--no-audio`` omits the audio m-line (M0-T4)."""
    lines = [
        "v=0",
        f"o=omascreencast {config.server_uuid} 0 IN IP4 {host}",
        "s=omascreencast M0 spike",
        "t=0 0",
        "a=recvonly",
        f"m=video 0 RTP/AVP 96",
        "a=rtpmap:96 H264/90000",
        "a=fmtp:96 sprop-parameter-sets=Z2QAH6w0yAT5oAQAAAAMAEAAAAwPI8UKZYA==,aO48gA==",
        "a=rtcp-fb:96 nack",
        "a=rtcp-fb:96 nack pli",
        "a=rtcp-fb:96 goog-remb",
        "a=content:main",
        "a=sendonly",
    ]
    if not config.no_audio:
        lines += [
            "m=audio 0 RTP/AVP 97",
            "a=rtpmap:97 OPUS/48000/2",
            "a=fmtp:97 surround-params=2",
            "a=sendonly",
        ]
    return "\r\n".join(lines) + "\r\n"


class GameStreamHandler(BaseHTTPRequestHandler):
    server_version = "omascreencast-m0-spike"
    protocol_version = "HTTP/1.1"

    # Set by make_server(); typed as Any to keep mypy out of the spike.
    config: ServerConfig
    request_log: RequestLog

    # -- plumbing ---------------------------------------------------------

    def log_message(self, fmt: str, *args: Any) -> None:  # noqa: D401
        """Route the stock access log to stderr, never to the capture file."""
        sys.stderr.write("spike: " + fmt % args + "\n")

    def _read_body(self) -> bytes:
        length = self.headers.get("Content-Length")
        if not length:
            return b""
        try:
            n = int(length)
        except ValueError:
            return b""
        return self.rfile.read(n) if n > 0 else b""

    def _record_request(self, path: str, body: bytes) -> dict[str, Any]:
        parsed = urlparse(path)
        entry = {
            "method": self.command,
            "request_line": self.raw_requestline.decode("latin-1").rstrip("\r\n"),
            "path": parsed.path,
            "query": {
                key: values if len(values) > 1 else values[0]
                for key, values in parse_qs(parsed.query, keep_blank_values=True).items()
            },
            "raw_query": parsed.query,
            "headers": [[name, value] for name, value in self.headers.items()],
            "client": {"address": self.client_address[0], "port": self.client_address[1]},
            "tls": isinstance(self.connection, ssl.SSLSocket),
            "body": _body_to_json(body, self.headers.get("Content-Type", "")),
        }
        self.request_log.record(entry)
        return entry

    def _respond(self, status: int, payload: Any, content_type: str = "application/json") -> None:
        if isinstance(payload, (dict, list)):
            body = json.dumps(payload).encode("utf-8")
        elif isinstance(payload, bytes):
            body = payload
        else:
            body = str(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Server", self.server_version)
        self.end_headers()
        self.wfile.write(body)

    # -- dispatch ---------------------------------------------------------

    def _dispatch(self) -> None:
        body = self._read_body()
        entry = self._record_request(self.path, body)
        route = entry["path"].rstrip("/") or "/"
        config = self.config

        if route in ("/", ""):
            self._respond(200, {"spike": SERVER_NAME, "route": "/"}, "application/json")
        elif route == "/serverinfo":
            self._respond(200, serverinfo(config))
        elif route == "/applist":
            self._respond(200, applist(config))
        elif route in ("/launch", "/resume"):
            self._respond(200, launch_response(config))
        elif route == "/cancel":
            self._respond(200, {})
        elif route == "/sdp":
            # Convenience for M0-T4: fetch the SDP over HTTP without RTSP.
            host = config.resolved_local_ip()
            self._respond(200, build_sdp(config, host), "application/sdp")
        elif route in (
            "/pair",
            "/unpair",
            "/getservercert",
            "/clientchallenge",
            "/serverchallengeresp",
            "/clientpairingsecret",
        ):
            # Pairing plumbing is intentionally a stub: the spike only records
            # the shape of the exchange. M1-T4 implements it for real.
            self._respond(200, {"spike": "pairing-not-implemented"})
        else:
            self._respond(404, {"error": "unknown route", "path": route})

    do_GET = _dispatch
    do_POST = _dispatch
    do_PUT = _dispatch
    do_DELETE = _dispatch

    def do_HEAD(self) -> None:  # pragma: no cover - not used by Moonlight
        self._record_request(self.path, b"")
        self.send_response(404)
        self.send_header("Content-Length", "0")
        self.end_headers()


def make_server(
    config: ServerConfig,
    tls_context: ssl.SSLContext | None = None,
    request_log: RequestLog | None = None,
) -> ThreadingHTTPServer:
    """Create a bound server. Port 0 in ``config`` requests an ephemeral port."""
    log = request_log or RequestLog(config.log_path)
    handler = type("BoundGameStreamHandler", (GameStreamHandler,), {"config": config, "request_log": log})
    httpd = ThreadingHTTPServer((config.bind, config.port), handler)
    httpd.daemon_threads = True
    if tls_context is not None:
        httpd.socket = tls_context.wrap_socket(httpd.socket, server_side=True)
    # Reflect the actual port back so ephemeral binds are usable.
    config.port = httpd.server_address[1]
    httpd.request_log = log  # type: ignore[attr-defined]
    return httpd


def ensure_certificate(cache_dir: Path) -> tuple[Path, Path]:
    """Generate a throwaway self-signed cert/key with ``openssl`` if absent."""
    cert = cache_dir / "spike-cert.pem"
    key = cache_dir / "spike-key.pem"
    if cert.is_file() and key.is_file():
        return cert, key
    cache_dir.mkdir(parents=True, exist_ok=True)
    try:
        subprocess.run(
            [
                "openssl",
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-days",
                "2",
                "-subj",
                f"/CN={SERVER_NAME}",
                "-keyout",
                str(key),
                "-out",
                str(cert),
            ],
            check=True,
            capture_output=True,
        )
    except FileNotFoundError as exc:  # pragma: no cover - openssl is ubiquitous
        raise SystemExit("spike: openssl not found; pass --cert and --key instead") from exc
    except subprocess.CalledProcessError as exc:  # pragma: no cover
        raise SystemExit(f"spike: openssl failed: {exc.stderr.decode(errors='replace')}") from exc
    return cert, key


def build_tls_context(cert: Path, key: Path) -> ssl.SSLContext:
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(certfile=str(cert), keyfile=str(key))
    # The spike accepts any client certificate; pairing is not implemented and
    # Moonlight's trust behaviour is exactly what M0 wants to observe.
    context.verify_mode = ssl.CERT_NONE
    return context


def advertise(config: ServerConfig, service_name: str | None = None) -> Any:
    """Publish ``_nvstream._tcp`` via zeroconf. Returns the Zeroconf handle."""
    try:
        from zeroconf import ServiceInfo, Zeroconf
    except ImportError as exc:  # pragma: no cover - depends on the host
        raise SystemExit(
            "spike: zeroconf is not installed. Install python-zeroconf, or run with "
            "--no-advertise and point Moonlight at the IP manually."
        ) from exc

    ip = config.resolved_local_ip()
    name = service_name or f"{config.hostname}._nvstream._tcp.local."
    info = ServiceInfo(
        "_nvstream._tcp.local.",
        name,
        addresses=[__import__("socket").inet_aton(ip)],
        port=config.port,
        properties={
            "txtvers": "1",
            "version": GFE_VERSION,
            "hostname": config.hostname,
            "uuid": config.server_uuid,
            "appversion": APP_VERSION,
            "GfeVersion": GFE_VERSION,
            "ht": "1",
            "IsPrerelease": "0",
        },
        server=f"{config.hostname}.local.",
    )
    zc = Zeroconf()
    zc.register_service(info)
    print(f"spike: advertising {name} -> {ip}:{config.port}", file=sys.stderr)
    return zc


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    repo_root = Path(__file__).resolve().parents[2]
    default_log = repo_root / "omascreencastd" / "tests" / "fixtures" / "m0" / "spike-capture.jsonl"
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bind", default="0.0.0.0", help="bind address (default: 0.0.0.0)")
    parser.add_argument("--port", type=int, default=DEFAULT_HTTPS_PORT, help="HTTPS port (0 = ephemeral)")
    parser.add_argument("--rtsp-port", type=int, default=DEFAULT_RTSP_PORT, help="RTSP port echoed to the client")
    parser.add_argument("--hostname", default=SERVER_NAME, help="advertised hostname")
    parser.add_argument("--uuid", default=None, help="server UUID (default: random per run)")
    parser.add_argument("--log", type=Path, default=default_log, help="JSONL capture path")
    parser.add_argument("--no-log", action="store_true", help="print captures to stderr instead of a file")
    parser.add_argument("--no-tls", action="store_true", help="serve plain HTTP (tests only)")
    parser.add_argument("--cert", type=Path, default=None, help="TLS certificate (default: generated)")
    parser.add_argument("--key", type=Path, default=None, help="TLS key (default: generated)")
    parser.add_argument("--no-advertise", action="store_true", help="skip mDNS advertisement")
    parser.add_argument("--no-audio", action="store_true", help="serve an SDP with no audio m-line (M0-T4)")
    parser.add_argument("--local-ip", default=None, help="override the IP used in responses")
    parser.add_argument("--seconds", type=float, default=None, help="stop after N seconds (0 = forever)")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    repo_root = Path(__file__).resolve().parents[2]
    cache_dir = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "omascreencast" / "m0-spike"

    config = ServerConfig(
        bind=args.bind,
        port=args.port,
        rtsp_port=args.rtsp_port,
        hostname=args.hostname,
        server_uuid=args.uuid or str(uuidlib.uuid4()),
        no_audio=args.no_audio,
        log_path=None if args.no_log else args.log,
        local_ip=args.local_ip,
    )

    tls: ssl.SSLContext | None = None
    if not args.no_tls:
        cert = args.cert
        key = args.key
        if cert is None or key is None:
            cert, key = ensure_certificate(cache_dir)
        tls = build_tls_context(cert, key)

    httpd = make_server(config, tls)
    scheme = "https" if tls else "http"
    print(f"spike: {scheme}://{config.resolved_local_ip()}:{config.port}/", file=sys.stderr)
    print(f"spike: capture -> {config.log_path or 'stderr'}", file=sys.stderr)
    if args.no_audio:
        print("spike: SDP advertised without an audio stream (M0-T4)", file=sys.stderr)

    zc = None
    if not args.no_advertise:
        try:
            zc = advertise(config)
        except SystemExit as exc:
            print(str(exc), file=sys.stderr)
            print("spike: continuing without mDNS", file=sys.stderr)

    def stop(_signum: int, _frame: Any) -> None:
        threading.Thread(target=httpd.shutdown, daemon=True).start()

    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGTERM, stop)

    if args.seconds is not None:
        def timer() -> None:
            time.sleep(args.seconds)
            if zc is not None:
                zc.unregister_all_services()
                zc.close()
            httpd.shutdown()

        threading.Thread(target=timer, daemon=True).start()

    try:
        httpd.serve_forever()
    finally:
        time.sleep(SHUTDOWN_GRACE_SECONDS)
        httpd.server_close()
        if zc is not None:
            try:
                zc.unregister_all_services()
            finally:
                zc.close()
        log: RequestLog = httpd.request_log  # type: ignore[attr-defined]
        log.close()
        print("spike: stopped", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
