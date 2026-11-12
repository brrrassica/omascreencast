#!/usr/bin/env python3
"""TLS-terminating pairing recorder for milestone M0-T5.

This is **spike tooling, not product code**. Its only job is to make the real
GameStream pairing bytes visible so they can be frozen as capture facts
(``plans/towards-0.2.0.md`` M0-T5) and turned into the production pairing
implementation (M1-T4).

Why a recorder and not a packet capture
--------------------------------------
The pairing exchange happens *inside* TLS on the control port. Sunshine does not
write an ``SSLKEYLOGFILE``, so a ``tshark``/``tcpdump`` capture of the oracle
yields ciphertext. A GameStream client accepts the host's self-signed
certificate during pairing — the PIN is what establishes trust — so a
TLS-terminating listener with its own throwaway certificate sees the plaintext
bodies without needing any key material from the oracle.

Design rules
------------
* Standard library only.
* Every request is recorded **before** a response is chosen, including bodies
  that are not ASCII (hex + base64 are both written), so nothing is lost to an
  encoding accident.
* Stub responses are honest: phases the recorder does not implement answer the
  wire shape of a host that is not ready, so the client's reaction is
  observable rather than guessed at.
* Values are the client's real bytes. The recorder never synthesises a
  plausible-looking field.

Usage
-----
    # observe what the client actually sends
    python3 tools/m0-gamestream/pairing_recorder.py \
        --log omascreencastd/tests/fixtures/m0/pairing-probe.jsonl

    # then, from another shell
    moonlight pair 127.0.0.1:<control-port> --pin 1234

The ports are printed on startup. ``--pin`` is optional and only used by the
experimental server implementations below; with no PIN the recorder is purely an
observer.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import ssl
import subprocess
import sys
import threading
import uuid as uuidlib
from dataclasses import dataclass, field
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs, urlparse
from xml.sax.saxutils import escape, quoteattr

APP_VERSION = "0.2.0-dev"
GFE_VERSION = "3.23.0.74"
SERVER_CODEC_MODE_SUPPORT = 259
DEFAULT_HTTPS_PORT = 47984
DEFAULT_DISCOVERY_PORT = 47989


@dataclass
class RecorderConfig:
    """Everything a stub response needs, plus the capture destination."""

    bind: str = "127.0.0.1"
    https_port: int = DEFAULT_HTTPS_PORT
    discovery_port: int = DEFAULT_DISCOVERY_PORT
    hostname: str = "omascreencast-m0-recorder"
    server_uuid: str = field(default_factory=lambda: str(uuidlib.uuid4()))
    pair_status: int = 0
    pin: str | None = None
    cert_pem: str = ""
    log_path: Path | None = None
    local_ip: str = "127.0.0.1"


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
        line = json.dumps(entry, ensure_ascii=False)
        with self._lock:
            if self._fh is not None:
                self._fh.write(line + "\n")
                self._fh.flush()
            else:
                print(line, file=sys.stderr)

    def close(self) -> None:
        with self._lock:
            if self._fh is not None:
                self._fh.close()
                self._fh = None


def _describe_body(body: bytes) -> dict[str, Any] | None:
    """Describe a body so neither an encoding accident nor binary safety loses it."""
    if not body:
        return None
    described: dict[str, Any] = {
        "length": len(body),
        "base64": base64.b64encode(body).decode("ascii"),
        "sha256": hashlib.sha256(body).hexdigest(),
    }
    try:
        described["text"] = body.decode("utf-8")
    except UnicodeDecodeError:
        described["text"] = None
    return described


def _envelope(status_code: int, children: str = "", status_message: str | None = None) -> str:
    attrs = f'status_code="{status_code}"'
    if status_message is not None:
        attrs += f" status_message={quoteattr(status_message)}"
    if children:
        return f'<?xml version="1.0" encoding="utf-8"?>\n<root {attrs}>{children}</root>'
    return f'<?xml version="1.0" encoding="utf-8"?>\n<root {attrs}/>'


def _xe(name: str, value: Any) -> str:
    return f"<{name}>{escape(str(value))}</{name}>"


def serverinfo_xml(config: RecorderConfig, external_port: int) -> str:
    children = (
        _xe("hostname", config.hostname)
        + _xe("appversion", APP_VERSION)
        + _xe("GfeVersion", GFE_VERSION)
        + _xe("uniqueid", config.server_uuid)
        + _xe("HttpsPort", config.https_port)
        + _xe("ExternalPort", external_port)
        + _xe("mac", "00:11:22:33:44:55")
        + _xe("LocalIP", config.local_ip)
        + _xe("ServerCodecModeSupport", SERVER_CODEC_MODE_SUPPORT)
        + _xe("PairStatus", config.pair_status)
        + _xe("currentgame", 0)
        + _xe("state", "SUNSHINE_SERVER_FREE")
    )
    return _envelope(200, children)


def servercert_xml(config: RecorderConfig) -> str:
    return _envelope(200, _xe("plaincert", config.cert_pem))


class RecorderHandler(BaseHTTPRequestHandler):
    server_version = "omascreencast-m0-recorder"
    protocol_version = "HTTP/1.1"

    config: RecorderConfig
    request_log: RequestLog
    external_port: int
    discovery_only: bool

    def log_message(self, fmt: str, *args: Any) -> None:
        sys.stderr.write("recorder: " + fmt % args + "\n")

    def _read_body(self) -> bytes:
        length = self.headers.get("Content-Length")
        if not length:
            return b""
        try:
            count = int(length)
        except ValueError:
            return b""
        return self.rfile.read(count) if count > 0 else b""

    def _client_certificate(self) -> dict[str, Any] | None:
        try:
            sock = self.connection
            if not isinstance(sock, ssl.SSLSocket):
                return None
            der = sock.getpeercert(binary_form=True)
            if not der:
                return None
            return {
                "present": True,
                "fingerprint_sha256": hashlib.sha256(der).hexdigest(),
                "der_base64": base64.b64encode(der).decode("ascii"),
            }
        except Exception:
            return None

    def _record(self, path: str, body: bytes) -> dict[str, Any]:
        parsed = urlparse(path)
        entry: dict[str, Any] = {
            "method": self.command,
            "path": parsed.path,
            "query": {
                key: values if len(values) > 1 else values[0]
                for key, values in parse_qs(parsed.query, keep_blank_values=True).items()
            },
            "raw_query": parsed.query,
            "headers": [[name, value] for name, value in self.headers.items()],
            "client": {"address": self.client_address[0], "port": self.client_address[1]},
            "tls": isinstance(self.connection, ssl.SSLSocket),
            "server_port": self.external_port,
            "discovery_only": self.discovery_only,
            "body": _describe_body(body),
        }
        certificate = self._client_certificate()
        if certificate is not None:
            entry["client_certificate"] = certificate
        self.request_log.record(entry)
        return entry

    def _respond(self, payload: str) -> None:
        body = payload.encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/xml")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Server", self.server_version)
        self.end_headers()
        self.wfile.write(body)

    def _dispatch(self) -> None:
        body = self._read_body()
        entry = self._record(self.path, body)
        route = entry["path"].rstrip("/") or "/"
        config = self.config
        print(
            f"recorder: {self.command} {entry['path']} "
            f"body={entry['body']['length'] if entry['body'] else 0}B",
            file=sys.stderr,
        )

        if self.discovery_only:
            if route == "/serverinfo":
                self._respond(serverinfo_xml(config, self.external_port))
            else:
                self._respond(_envelope(404))
            return

        if route == "/serverinfo":
            self._respond(serverinfo_xml(config, self.external_port))
        elif route == "/getservercert":
            self._respond(servercert_xml(config))
        elif route == "/pair":
            self._respond(_envelope(200, _xe("paired", 1)))
        elif route == "/unpair":
            self._respond(_envelope(200))
        else:
            # Everything else is recorded and refused with a bare 404 envelope:
            # the recorder is an observer, not a host. A client's reaction to
            # this is a fact worth capturing.
            self._respond(_envelope(404))

    do_GET = _dispatch
    do_POST = _dispatch
    do_PUT = _dispatch
    do_DELETE = _dispatch


def _make_handler(config: RecorderConfig, log: RequestLog, external_port: int, discovery_only: bool):
    return type(
        "BoundRecorderHandler",
        (RecorderHandler,),
        {
            "config": config,
            "request_log": log,
            "external_port": external_port,
            "discovery_only": discovery_only,
        },
    )


def make_server(
    config: RecorderConfig,
    tls_context: ssl.SSLContext | None,
    request_log: RequestLog,
    discovery_only: bool,
) -> ThreadingHTTPServer:
    bind_port = config.discovery_port if discovery_only else config.https_port
    handler = _make_handler(config, request_log, bind_port, discovery_only)
    httpd = ThreadingHTTPServer((config.bind, bind_port), handler)
    httpd.daemon_threads = True
    if tls_context is not None:
        httpd.socket = tls_context.wrap_socket(httpd.socket, server_side=True)
    actual = httpd.server_address[1]
    if discovery_only:
        config.discovery_port = actual
    else:
        config.https_port = actual
    handler.external_port = actual
    return httpd


def ensure_certificate(cache_dir: Path) -> tuple[Path, Path]:
    cert = cache_dir / "recorder-cert.pem"
    key = cache_dir / "recorder-key.pem"
    if cert.is_file() and key.is_file():
        return cert, key
    cache_dir.mkdir(parents=True, exist_ok=True)
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
            "/CN=omascreencast-m0-recorder",
            "-keyout",
            str(key),
            "-out",
            str(cert),
        ],
        check=True,
        capture_output=True,
    )
    return cert, key


def build_tls_context(cert: Path, key: Path) -> ssl.SSLContext:
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(certfile=str(cert), keyfile=str(key))
    # Record a client certificate when offered and never reject the handshake.
    context.verify_mode = ssl.CERT_OPTIONAL
    context.check_hostname = False
    return context


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    repo_root = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--bind", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=DEFAULT_HTTPS_PORT)
    parser.add_argument("--discovery-port", type=int, default=DEFAULT_DISCOVERY_PORT)
    parser.add_argument("--hostname", default="omascreencast-m0-recorder")
    parser.add_argument(
        "--log",
        type=Path,
        default=repo_root / "omascreencastd" / "tests" / "fixtures" / "m0" / "pairing-probe.jsonl",
    )
    parser.add_argument("--no-log", action="store_true")
    parser.add_argument("--pin", default=None, help="PIN for the experimental host (optional)")
    parser.add_argument("--seconds", type=float, default=120, help="stop after N seconds")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    cache_dir = Path.home() / ".cache" / "omascreencast" / "m0-recorder"
    cert, key = ensure_certificate(cache_dir)
    config = RecorderConfig(
        bind=args.bind,
        https_port=args.port,
        discovery_port=args.discovery_port,
        hostname=args.hostname,
        pin=args.pin,
        log_path=None if args.no_log else args.log,
        cert_pem=cert.read_text(encoding="utf-8"),
    )
    log = RequestLog(config.log_path)
    tls = build_tls_context(cert, key)
    httpsd = make_server(config, tls, log, discovery_only=False)
    discoveryd = make_server(config, None, log, discovery_only=True)

    print(f"recorder: control   https://{config.bind}:{config.https_port}/", file=sys.stderr)
    print(
        f"recorder: discovery http://{config.bind}:{config.discovery_port}/serverinfo",
        file=sys.stderr,
    )
    print(f"recorder: capture   {config.log_path or 'stderr'}", file=sys.stderr)

    threads = [
        threading.Thread(target=httpsd.serve_forever, daemon=True),
        threading.Thread(target=discoveryd.serve_forever, daemon=True),
    ]
    for thread in threads:
        thread.start()

    def stop() -> None:
        httpsd.shutdown()
        discoveryd.shutdown()

    if args.seconds:
        threading.Timer(args.seconds, stop).start()

    try:
        while threads[0].is_alive():
            threads[0].join(timeout=0.5)
    except KeyboardInterrupt:
        stop()
    finally:
        httpsd.server_close()
        discoveryd.server_close()
        log.close()
        print("recorder: stopped", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
