#!/usr/bin/env python3
"""Dev server for the fab-web bundle: localhost with PROD-shaped headers — COOP/COEP (so
crossOriginIsolated behaves like the special page), application/wasm (so instantiateStreaming
takes the fast path) and no-store (so iteration never fights the browser cache).
Usage: dev-server.py [dir] [port=8787]."""

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


if __name__ == "__main__":
    directory = sys.argv[1] if len(sys.argv) > 1 else "."
    port = int(sys.argv[2]) if len(sys.argv) > 2 else 8787
    print(f"fab-web dev server: http://127.0.0.1:{port}/ (serving {directory}, COOP/COEP on)")
    http.server.ThreadingHTTPServer(
        ("127.0.0.1", port), functools.partial(Handler, directory=directory)
    ).serve_forever()
