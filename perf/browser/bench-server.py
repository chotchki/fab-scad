#!/usr/bin/env python3
"""W.3.17 bench server: dev-server.py's COOP/COEP static serving (SharedArrayBuffer → fab threads on) +
a POST /__result sink so the harness reports results SERVER-SIDE. Headless-Chrome console routing is
unreliable (the repo's e2e treats a server record, not console grep, as ground truth) — so the harness
POSTs its final JSON here and the driver polls the file. Usage: bench-server.py <dir> <port> <result_file>."""
import functools
import http.server
import sys


class Handler(http.server.SimpleHTTPRequestHandler):
    extensions_map = {
        **http.server.SimpleHTTPRequestHandler.extensions_map,
        ".wasm": "application/wasm",
        ".js": "text/javascript",
    }

    def end_headers(self):
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def do_POST(self):
        if self.path == "/__result":
            n = int(self.headers.get("Content-Length", 0))
            body = self.rfile.read(n)
            with open(RESULT_FILE, "wb") as f:
                f.write(body)
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"ok")
        else:
            self.send_response(404)
            self.end_headers()

    def log_message(self, *a):
        pass  # quiet — the driver scrapes the result file, not the access log


if __name__ == "__main__":
    directory = sys.argv[1]
    port = int(sys.argv[2])
    RESULT_FILE = sys.argv[3]
    print(f"bench server http://127.0.0.1:{port}/ dir={directory} result={RESULT_FILE}")
    http.server.ThreadingHTTPServer(
        ("127.0.0.1", port), functools.partial(Handler, directory=directory)
    ).serve_forever()
