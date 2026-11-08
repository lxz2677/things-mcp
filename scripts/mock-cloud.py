#!/usr/bin/env python3
"""Minimal Things Cloud mock for end-to-end write-path testing.

Implements just enough of /version/1 to exercise auth, pull, commit, cursor
tracking and replay. Each commit becomes one history batch (one index).
"""
import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

EMAIL = "me@example.com"
HISTORY = []  # list of batches: {uuid: {"t":0|1|2,"e":...,"p":{...}}}
LOCK = threading.Lock()


def head():
    return len(HISTORY)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def _send(self, code, obj=None, extra_headers=None):
        body = b"" if obj is None else json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        for key, value in (extra_headers or {}).items():
            self.send_header(key, value)
        self.end_headers()
        if body:
            self.wfile.write(body)

    def _authorized(self):
        return (self.headers.get("Authorization") or "").startswith("Password ")

    def do_GET(self):
        parsed = urlparse(self.path)
        path = parsed.path
        query = parse_qs(parsed.query)

        if not self._authorized():
            return self._send(401, {"error": "unauthorized"},
                              {"things-response": "Unauthorized"})

        if path == f"/version/1/account/{EMAIL}":
            return self._send(200, {
                "SLA-version-accepted": "5",
                "email": EMAIL,
                "history-key": "HK",
                "issues": [],
                "maildrop-email": None,
                "status": "SYAccountStatusActive",
            })
        if path == f"/version/1/account/{EMAIL}/own-history-key":
            return self._send(200, {
                "history-key": "HK",
                "is-empty": head() == 0,
                "latest-schema-version": 301,
                "latest-server-index": head(),
            })
        if path == "/version/1/history/HK":
            return self._send(200, {
                "is-empty": head() == 0,
                "latest-schema-version": 301,
                "latest-server-index": head(),
                "latest-total-content-size": 0,
            })
        if path == "/version/1/history/HK/items":
            start = int(query.get("start-index", ["0"])[0])
            if start > head():
                return self._send(500, {"error": "start-index beyond head"})
            return self._send(200, {
                "items": HISTORY[start:],
                "current-item-index": head(),
                "schema": 301,
                "latest-total-content-size": 0,
                "start-total-content-size": 0,
                "end-total-content-size": 0,
            })

        return self._send(404, {"error": "not found"})

    def do_POST(self):
        parsed = urlparse(self.path)
        path = parsed.path
        query = parse_qs(parsed.query)
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b""
        body = json.loads(raw or b"{}")

        if not self._authorized():
            return self._send(401, {"error": "unauthorized"},
                              {"things-response": "Unauthorized"})

        if path == "/version/1/history/HK/commit":
            with LOCK:
                ancestor = int(query.get("ancestor-index", ["-1"])[0])
                if ancestor != head():
                    return self._send(409, {"error": "OutdatedAncestor"})
                if not isinstance(body, dict) or not body:
                    return self._send(400, {"error": "empty change map"})
                for uuid, item in body.items():
                    if item.get("e") not in {
                        "Task7", "ChecklistItem3", "Area3", "Tag4",
                        "Settings5", "Tombstone2",
                    }:
                        return self._send(400, {"error": f"bad entity {item.get('e')}"})
                HISTORY.append(body)
                return self._send(200, {"server-head-index": head()})

        return self._send(404, {"error": "not found"})


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 18081
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    print(f"mock cloud listening on {port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
