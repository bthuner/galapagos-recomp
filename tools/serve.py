#!/usr/bin/env python3
"""Serve web/ for development, telling the browser to revalidate every file.

`python3 -m http.server` sends no Cache-Control header, so browsers guess how
long a file stays fresh (often hours for a file that hasn't changed in a
while) and keep running yesterday's JavaScript after you pull. That is how a
merged feature can "not show up". With `Cache-Control: no-cache` the browser
still caches, but asks the server first; an unchanged file costs a 304.

Usage: python3 tools/serve.py [port]   (default 8000, serves ./web)
"""
import functools
import http.server
import pathlib
import sys


class NoCache(http.server.SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header("Cache-Control", "no-cache")
        super().end_headers()


port = int(sys.argv[1]) if len(sys.argv) > 1 else 8000
web = pathlib.Path(__file__).resolve().parent.parent / "web"
handler = functools.partial(NoCache, directory=str(web))
print(f"serving {web} on http://localhost:{port} (Cache-Control: no-cache)")
http.server.ThreadingHTTPServer(("", port), handler).serve_forever()
