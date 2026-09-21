#!/usr/bin/env python3
"""Synthetic Shoutcast v1 server for stall/reconnect testing: answers every request with an
`ICY 200 OK` status line (which hyper rejects) and then streams the given file in a loop.

Promoted from the M3 Step 0 census (2026-09-21), where it measured that the engine sent an ICY
server into the reconnect loop instead of reporting `Http`; the unit test
`stream::tests::icy_status_line_is_http_not_network` now pins the fix, and this script is the
manual end-to-end check:

    python3 scripts/icy-server.py 18765 path/to/any.mp3
    cargo run -p ondar-audio --example stall_bench -- http://127.0.0.1:18765/stream 12
    # expected: state Error { code: Http, .. } on the first attempt, no Reconnecting
"""
import socket, sys, time
if len(sys.argv) != 3:
    sys.exit("usage: icy-server.py <port> <audio file to loop>")
port = int(sys.argv[1]); data = open(sys.argv[2], "rb").read()
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(("127.0.0.1", port)); s.listen(4)
print(f"icy-server listening 127.0.0.1:{port}", flush=True)
while True:
    c, a = s.accept()
    req = c.recv(4096)
    first = req.split(b"\r\n")[0]
    print(f"{time.strftime('%H:%M:%S')} request from {a}: {first!r}", flush=True)
    try:
        c.sendall(b"ICY 200 OK\r\nicy-name: synthetic shoutcast v1\r\nicy-br: 128\r\ncontent-type: audio/mpeg\r\n\r\n")
        while True:
            c.sendall(data); time.sleep(0.5)
    except Exception as e:
        print(f"client gone: {e}", flush=True)
    finally:
        c.close()
