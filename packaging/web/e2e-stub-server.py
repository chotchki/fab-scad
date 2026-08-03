#!/usr/bin/env python3
"""Stub hotchkiss.io for the W.5.9 save-back e2e (docs/web-save-back.md).

Serves the fab-gui bundle with PROD-shaped headers (COOP/COEP so the threaded geom worker's
SharedArrayBuffer instantiates; application/wasm for the streaming path) AND stands in for the
site's media resource so the round-trip closes WITHOUT the real server:

  GET  /media/<ref>[?format=scad]   -> the demo .scad (application/x-openscad), or --model's file.
                                       The load half — the app fetch_text's the ?model= URL.
                                       Set-Cookie plants a session cookie so the same-origin PUT
                                       carries it (auth wiring proof).
  PUT  /media/<ref>/variants        -> the SAVE half. Parses the multipart, records each file part
                                       (name/filename/ext/bytes) + whether the session cookie rode,
                                       writes a JSON summary to --record, and 200s a manifest.
                                       With --dump, ALSO writes each part's BYTES to <dir>/<name>.<ext>.
  GET  /__e2e/state                 -> that recorded summary (or {}), for the runner to assert on.

Everything else falls through to static file serving out of <dir>. stdlib only.
Usage: e2e-stub-server.py <dir> [port=8788] [--record PATH] [--model FILE] [--dump DIR]

--model and --dump exist for the SZ.9.3 parity gate: serve a chosen model, then keep the mesh the
browser produced so it can be diffed against the desktop's for the same source. Without --dump the
stub records only byte COUNTS, which is enough to prove a save round-tripped and nothing at all
about whether the geometry was right.
"""

import http.server
import json
import os
import sys

# A tiny COLORED model so the save path exercises the colored-3MF arm (vertex_colors -> both 3MF),
# not just STL. Plain cube, no includes, so it renders fast under software WebGL in CI.
DEMO_SCAD = b'// fab-gui save-back e2e fixture\ncolor("red") cube(20, center=true);\n'
SESSION_COOKIE = "id=e2e-session"


def _parse_multipart(body: bytes, content_type: str):
    """Pull (name, filename, ext, bytes, content_type) out of a multipart/form-data body — enough to
    assert the three variants arrived, correctly typed by extension. Manual split (stdlib `cgi` is
    gone in 3.13); tolerant of the browser's exact framing."""
    marker = "boundary="
    i = content_type.find(marker)
    if i < 0:
        return []
    boundary = content_type[i + len(marker):].strip().strip('"')
    sep = b"--" + boundary.encode()
    parts = []
    for chunk in body.split(sep):
        if not chunk or chunk in (b"--\r\n", b"--", b"\r\n"):
            continue
        chunk = chunk[2:] if chunk.startswith(b"\r\n") else chunk
        head_end = chunk.find(b"\r\n\r\n")
        if head_end < 0:
            continue
        headers = chunk[:head_end].decode("latin-1", "replace")
        data = chunk[head_end + 4:]
        if data.endswith(b"\r\n"):
            data = data[:-2]
        disp = next((h for h in headers.splitlines() if h.lower().startswith("content-disposition")), "")
        if "filename=" not in disp:
            continue  # a non-file field — the server ignores these anyway
        name = _kv(disp, "name")
        filename = _kv(disp, "filename")
        ctype = next((h.split(":", 1)[1].strip() for h in headers.splitlines()
                      if h.lower().startswith("content-type")), "")
        ext = filename.rsplit(".", 1)[-1].lower() if "." in filename else ""
        # `_data` is stripped before the summary is serialized — it exists only for --dump.
        parts.append({"name": name, "filename": filename, "ext": ext,
                      "bytes": len(data), "content_type": ctype, "_data": data})
    return parts


def _kv(disp: str, key: str) -> str:
    tag = key + '="'
    i = disp.find(tag)
    if i < 0:
        return ""
    j = disp.find('"', i + len(tag))
    return disp[i + len(tag):j] if j > 0 else ""


class Handler(http.server.SimpleHTTPRequestHandler):
    record_path = "/tmp/fab-e2e-put.json"
    model_bytes = DEMO_SCAD   # --model overrides
    dump_dir = None           # --dump: where to write the received part bytes

    extensions_map = {
        **http.server.SimpleHTTPRequestHandler.extensions_map,
        ".wasm": "application/wasm",
        ".js": "text/javascript",
    }

    def end_headers(self):
        # Cross-origin isolation (SharedArrayBuffer) + a session cookie every response re-plants, so a
        # same-origin credentialed PUT is authed like the real site (SameSite=Lax rides a same-site PUT).
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cross-Origin-Resource-Policy", "cross-origin")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Set-Cookie", f"{SESSION_COOKIE}; Path=/; SameSite=Lax")
        super().end_headers()

    def _send_json(self, code: int, obj):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        path = self.path.split("?", 1)[0]
        if path == "/__e2e/state":
            try:
                with open(self.record_path) as f:
                    return self._send_json(200, json.load(f))
            except (OSError, ValueError):
                return self._send_json(200, {})
        # The negotiated model read: any /media/<ref> that isn't the byte route -> the demo scad. (The
        # real site 307s ?format=scad to the source; the app only needs the text, so serve it directly.)
        if path.startswith("/media/") and not path.startswith("/media/file/"):
            self.send_response(200)
            self.send_header("Content-Type", "application/x-openscad")
            self.send_header("Content-Length", str(len(self.model_bytes)))
            self.end_headers()
            self.wfile.write(self.model_bytes)
            return
        return super().do_GET()

    def do_PUT(self):
        path = self.path.split("?", 1)[0]
        if not (path.startswith("/media/") and path.endswith("/variants")):
            return self._send_json(404, {"error": "not a variant collection"})
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length) if length else b""
        parts = _parse_multipart(body, self.headers.get("Content-Type", ""))
        cookie = self.headers.get("Cookie", "")
        exts = [p["ext"] for p in parts]
        mesh_exts = [e for e in exts if e in ("stl", "3mf")]

        # --dump: keep the BYTES, keyed by the part's form-field name (`source`/`low`/`high`/`plate`),
        # so `high.3mf` is the full-res mesh the browser actually produced. The SZ.9.3 gate then diffs
        # it against a desktop render of the same source — the only place in the tree where the two
        # platforms' geometry is ever compared rather than assumed identical.
        if self.dump_dir:
            try:
                os.makedirs(self.dump_dir, exist_ok=True)
                for p in parts:
                    name = p["name"] or "part"
                    out = os.path.join(self.dump_dir, f"{name}.{p['ext']}" if p["ext"] else name)
                    with open(out, "wb") as f:
                        f.write(p["_data"])
            except OSError as e:
                sys.stderr.write(f"e2e-stub: could not dump parts: {e}\n")

        # Strip the bytes — they are not JSON-serializable and the record is a summary.
        parts = [{k: v for k, v in p.items() if k != "_data"} for p in parts]
        summary = {
            "method": "PUT",
            "ref": path[len("/media/"):-len("/variants")],
            "count": len(parts),
            "parts": parts,
            "cookie_present": SESSION_COOKIE in cookie,
            "has_scad": "scad" in exts,
            "mesh_ext_match": len(mesh_exts) == 2 and mesh_exts[0] == mesh_exts[1],
        }
        try:
            with open(self.record_path, "w") as f:
                json.dump(summary, f)
        except OSError as e:
            sys.stderr.write(f"e2e-stub: could not write record: {e}\n")
        # Mimic the site's 200 + item manifest (the app only reads that it's 2xx).
        mesh = mesh_exts[0] if mesh_exts else "stl"
        self._send_json(200, {
            "ref": summary["ref"],
            "kind": "stl",
            "variants": [{"type": "application/x-openscad" if p["ext"] == "scad"
                          else f"model/{mesh}", "bytes": p["bytes"]} for p in parts],
        })

    def log_message(self, fmt, *args):  # quiet — the runner greps Chrome's console, not this
        pass


def main():
    directory = sys.argv[1] if len(sys.argv) > 1 else "."
    port = 8788
    record = Handler.record_path
    model = None
    dump = None
    rest = sys.argv[2:]
    i = 0
    while i < len(rest):
        if rest[i] == "--record" and i + 1 < len(rest):
            record = rest[i + 1]
            i += 2
        elif rest[i] == "--model" and i + 1 < len(rest):
            model = rest[i + 1]
            i += 2
        elif rest[i] == "--dump" and i + 1 < len(rest):
            dump = rest[i + 1]
            i += 2
        else:
            port = int(rest[i])
            i += 1
    Handler.record_path = record
    Handler.dump_dir = dump
    if model:
        # Read eagerly and FAIL here if it is unreadable. A stub that silently fell back to the demo
        # cube would let the parity gate compare a cube against a BOSL2 render and report a mismatch
        # that has nothing to do with the platforms.
        with open(model, "rb") as f:
            Handler.model_bytes = f.read()
        print(f"fab-gui e2e stub: serving {model} ({len(Handler.model_bytes)} bytes) as the model")
    # A fresh run must not see a stale PUT record.
    try:
        os.remove(record)
    except OSError:
        pass
    print(f"fab-gui e2e stub: http://127.0.0.1:{port}/ (serving {directory}, record -> {record})")
    import functools
    http.server.ThreadingHTTPServer(
        ("127.0.0.1", port), functools.partial(Handler, directory=directory)
    ).serve_forever()


if __name__ == "__main__":
    main()
