#!/usr/bin/env python3
"""Deterministic stall / reconnect test server for Ondar's audio pipeline.

Loops an MP3 file at real-time rate over a raw socket (not http.server — we need control of
the status line, headers, and half-open/reset/hang behaviour that http.server hides). See
README "Stall testing" for the procedure this feeds and what each mode is meant to exercise.

Usage:
    python3 scripts/stall-server.py --file loop.mp3 --mode stall --after 8 --hold 20

No third-party dependencies.
"""

from __future__ import annotations

import argparse
import itertools
import socket
import struct
import sys
import threading
import time
from datetime import datetime, timezone
from typing import Dict, List, Optional, Tuple

_conn_ids = itertools.count(1)
_log_lock = threading.Lock()


def log(msg: str) -> None:
    ts = datetime.now(timezone.utc).isoformat(timespec="milliseconds")
    with _log_lock:
        print(f"{ts} {msg}", flush=True)


def build_metadata_block(title: str) -> bytes:
    """ICY in-band metadata: one length byte (units of 16 bytes) + the NUL-padded text."""
    text = f"StreamTitle='{title}';".encode("utf-8")
    pad = (-len(text)) % 16
    text += b"\x00" * pad
    length_byte = len(text) // 16
    return bytes([length_byte]) + text


def parse_range_start(value: Optional[str]) -> Optional[int]:
    if not value:
        return None
    value = value.strip()
    if not value.lower().startswith("bytes="):
        return None
    spec = value[len("bytes="):].split(",")[0].strip()
    start_str, _, _end_str = spec.partition("-")
    try:
        return int(start_str)
    except ValueError:
        return None


def parse_request(sock: socket.socket) -> Optional[Tuple[str, Dict[str, str]]]:
    """Read a request line + headers by hand. Returns None on EOF/garbage/timeout."""
    buf = b""
    sock.settimeout(10)
    try:
        while b"\r\n\r\n" not in buf:
            chunk = sock.recv(4096)
            if not chunk:
                return None
            buf += chunk
            if len(buf) > 65536:
                return None
    except OSError:
        return None
    finally:
        sock.settimeout(None)

    head, _, _ = buf.partition(b"\r\n\r\n")
    lines = head.decode("iso-8859-1").split("\r\n")
    if not lines or not lines[0]:
        return None
    request_line = lines[0]
    headers: Dict[str, str] = {}
    for line in lines[1:]:
        if ":" in line:
            k, _, v = line.partition(":")
            headers[k.strip().lower()] = v.strip()
    return request_line, headers


class Server:
    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        with open(args.file, "rb") as f:
            self.data = f.read()
        if not self.data:
            raise SystemExit(f"{args.file} is empty")
        # bitrate_kbps * 1000 bits/s / 8 bits/byte / 10 slices-per-second
        self.slice_bytes = max(1, args.bitrate * 1000 // 8 // 10)
        self.titles = [t.strip() for t in args.titles.split(",") if t.strip()] or ["Stall Test"]

    def current_title(self, elapsed: float) -> str:
        idx = int(elapsed // self.args.title_every) % len(self.titles)
        return self.titles[idx]

    def serve_forever(self) -> None:
        srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        srv.bind(("127.0.0.1", self.args.port))
        srv.listen(8)
        log(
            f"listening on 127.0.0.1:{self.args.port} mode={self.args.mode} "
            f"range={self.args.range} bitrate={self.args.bitrate}kbps "
            f"file={self.args.file} ({len(self.data)}B)"
        )
        try:
            while True:
                sock, addr = srv.accept()
                cid = next(_conn_ids)
                threading.Thread(
                    target=self.handle, args=(sock, addr, cid), daemon=True
                ).start()
        except KeyboardInterrupt:
            pass
        finally:
            srv.close()

    # -- one connection --------------------------------------------------

    def handle(self, sock: socket.socket, addr: Tuple[str, int], cid: int) -> None:
        start = time.monotonic()
        # A box, not a plain local: stream_body's sends update this in place, so a byte count
        # already sent survives even if a later send raises mid-connection.
        sent_box = [0]
        reason = "unknown"
        try:
            parsed = parse_request(sock)
            if parsed is None:
                log(f"conn={cid} peer={addr[0]}:{addr[1]} malformed or empty request")
                reason = "malformed-request"
                return
            request_line, headers = parsed
            range_hdr = headers.get("range")
            icy_meta_hdr = headers.get("icy-metadata")
            ua = headers.get("user-agent", "-")
            log(
                f"conn={cid} peer={addr[0]}:{addr[1]} request={request_line!r} "
                f"range={range_hdr!r} icy-metadata={icy_meta_hdr!r} user-agent={ua!r}"
            )

            if self.args.mode == "hang":
                # Never send a status line at all; just hold the accepted connection.
                time.sleep(self.args.hold)
                reason = "hang-hold-timeout"
                return

            if range_hdr and self.args.range == "reject":
                self.send_416(sock)
                reason = "416-reject"
                return

            start_offset = 0
            status_line = "ICY 200 OK" if self.args.mode == "icy200" else "HTTP/1.0 200 OK"
            extra_headers: List[str] = []
            if self.args.range in ("honour", "reject"):
                # Advertised on every response (including this one), since stream-download
                # reads Accept-Ranges once from the *first* response and caches the decision.
                extra_headers.append("Accept-Ranges: bytes")

            if range_hdr and self.args.range == "honour":
                requested = parse_range_start(range_hdr)
                start_offset = (requested or 0) % len(self.data)
                status_line = "HTTP/1.0 206 Partial Content"
                extra_headers.append(
                    f"Content-Range: bytes {start_offset}-{len(self.data) - 1}/{len(self.data)}"
                )

            want_metaint = self.args.mode == "metaint" and icy_meta_hdr == "1"
            if want_metaint:
                extra_headers.append(f"icy-metaint: {self.args.icy_metaint}")

            self.send_status_and_headers(sock, status_line, extra_headers)
            reason = self.stream_body(sock, start, start_offset, want_metaint, sent_box)
        except (BrokenPipeError, ConnectionResetError):
            reason = "client-disconnect"
        except OSError as e:
            reason = f"socket-error({e})"
        finally:
            elapsed = time.monotonic() - start
            log(f"conn={cid} end sent={sent_box[0]}B elapsed={elapsed:.2f}s reason={reason}")
            try:
                sock.close()
            except OSError:
                pass

    def send_416(self, sock: socket.socket) -> None:
        lines = [
            "HTTP/1.0 416 Range Not Satisfiable",
            f"Content-Range: bytes */{len(self.data)}",
            "Connection: close",
        ]
        sock.sendall(("\r\n".join(lines) + "\r\n\r\n").encode("ascii"))

    def send_status_and_headers(
        self, sock: socket.socket, status_line: str, extra_headers: List[str]
    ) -> None:
        lines = [
            status_line,
            "icy-name: Stall Test",
            f"icy-br: {self.args.bitrate}",
            "Content-Type: audio/mpeg",
            *extra_headers,
            "Connection: close",
        ]
        sock.sendall(("\r\n".join(lines) + "\r\n\r\n").encode("ascii"))

    def stream_body(
        self,
        sock: socket.socket,
        conn_start: float,
        start_offset: int,
        want_metaint: bool,
        sent_box: List[int],
    ) -> str:
        # `sent_box[0]` (not a local) so bytes already sent are still logged correctly if a
        # later `sendall` raises mid-connection (e.g. the peer vanished mid-burst).
        args = self.args
        data = self.data
        n = len(data)
        cursor = start_offset % n
        metaint_counter = 0  # bytes sent since the last metadata block

        def take(count: int) -> bytes:
            nonlocal cursor
            out = bytearray()
            remaining = count
            while remaining > 0:
                space = n - cursor
                chunk_len = min(space, remaining)
                out += data[cursor : cursor + chunk_len]
                cursor = (cursor + chunk_len) % n
                remaining -= chunk_len
            return bytes(out)

        def raw_send(chunk: bytes) -> None:
            nonlocal metaint_counter
            if not want_metaint:
                sock.sendall(chunk)
                sent_box[0] += len(chunk)
                return
            offset = 0
            while offset < len(chunk):
                to_boundary = args.icy_metaint - metaint_counter
                take_len = min(to_boundary, len(chunk) - offset)
                sock.sendall(chunk[offset : offset + take_len])
                sent_box[0] += take_len
                metaint_counter += take_len
                offset += take_len
                if metaint_counter >= args.icy_metaint:
                    title = self.current_title(time.monotonic() - conn_start)
                    sock.sendall(build_metadata_block(title))
                    log(
                        f"metadata title={title!r} at={sent_box[0]}B "
                        f"elapsed={time.monotonic() - conn_start:.2f}s"
                    )
                    metaint_counter = 0

        if args.burst_bytes > 0:
            raw_send(take(args.burst_bytes))

        # Absolute-schedule pacing. Sleeping `interval - time_spent_sending` looks like it
        # compensates, and it does compensate for `sendall` — but every deadline is derived
        # from "now", so `time.sleep` overshoot (1-3 ms on macOS) and the uncompensated loop
        # head accumulate against no fixed reference. Measured: 15,429 B/s against a nominal
        # 16,000 B/s, 3.6% slow, which surfaced as a spurious ~3%-per-10s decay in ICY
        # freshness and invalidated an earlier latency table. Anchoring each deadline to
        # `paced_start` and keying it on bytes actually sent makes the error bounded rather
        # than cumulative. Keyed on bytes, not slice count, so `slice_bytes` rounding at odd
        # bitrates cannot skew the rate. The burst stays outside the schedule: it is
        # deliberate head start.
        bytes_per_sec = args.bitrate * 1000 / 8
        paced_start = time.monotonic()
        paced_bytes = 0

        cutoff_hit = False
        while True:
            elapsed = time.monotonic() - conn_start
            if args.mode in ("stall", "close", "reset") and not cutoff_hit and elapsed >= args.after:
                cutoff_hit = True
                if args.mode == "stall":
                    time.sleep(args.hold)
                    return "stall-hold-timeout"
                if args.mode == "close":
                    try:
                        sock.shutdown(socket.SHUT_WR)
                    except OSError:
                        pass
                    return "close"
                if args.mode == "reset":
                    sock.setsockopt(
                        socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0)
                    )
                    sock.close()
                    return "reset"

            raw_send(take(self.slice_bytes))
            paced_bytes += self.slice_bytes
            deadline = paced_start + paced_bytes / bytes_per_sec
            now = time.monotonic()
            if deadline > now:
                time.sleep(deadline - now)


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--file", required=True, help="MP3 file to loop")
    p.add_argument("--port", type=int, default=8765)
    p.add_argument("--bitrate", type=int, default=128, help="kbps; paces body sends")
    p.add_argument("--burst-bytes", type=int, default=65536)
    p.add_argument(
        "--mode",
        choices=["stall", "close", "reset", "icy200", "metaint", "hang"],
        default="stall",
    )
    p.add_argument(
        "--after", type=float, default=15.0, help="seconds before stall/close/reset cutoff"
    )
    p.add_argument(
        "--hold", type=float, default=60.0, help="seconds to hold the socket open (stall, hang)"
    )
    p.add_argument("--range", choices=["honour", "ignore", "reject"], default="ignore")
    p.add_argument("--icy-metaint", type=int, default=16000)
    p.add_argument("--titles", default="Track One,Track Two,Track Three")
    p.add_argument("--title-every", type=float, default=10.0)
    return p.parse_args()


def main() -> None:
    args = parse_args()
    if args.bitrate <= 0:
        raise SystemExit("--bitrate must be positive")
    if sys.version_info < (3, 7):
        raise SystemExit("Python >= 3.7 required")
    Server(args).serve_forever()


if __name__ == "__main__":
    main()
