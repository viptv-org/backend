#!/usr/bin/env python3
"""Historical generic HTTPS byte-range fixture; no backend runtime/packaging hook.

Retained by gateway-owner coordination for possible progressive-reader diagnostics.
It serves one explicitly supplied directory; real certificates/keys stay private.
"""
import argparse
import http.server
import pathlib
import ssl
import urllib.parse

p = argparse.ArgumentParser()
p.add_argument('directory', type=pathlib.Path)
p.add_argument('--cert', required=True)
p.add_argument('--key', required=True)
p.add_argument('--port', type=int, default=18795)
a = p.parse_args()

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_HEAD(self): self.do_GET()
    def do_GET(self):
        name = urllib.parse.urlsplit(self.path).path.removeprefix('/')
        if not name or '/' in name or name in ('.', '..'):
            self.send_error(404); return
        file = a.directory / name
        if not file.is_file(): self.send_error(404); return
        size = file.stat().st_size
        start, end = 0, size - 1
        value = self.headers.get('Range', '')
        if value.startswith('bytes='):
            first, last = value[6:].split('-', 1)
            start = int(first or 0); end = min(end, int(last) if last else end)
        if start > end: self.send_error(416); return
        self.send_response(206 if value else 200)
        self.send_header('Accept-Ranges', 'bytes')
        self.send_header('Content-Type', 'application/octet-stream')
        self.send_header('Content-Length', end - start + 1)
        if value: self.send_header('Content-Range', f'bytes {start}-{end}/{size}')
        self.end_headers()
        if self.command == 'HEAD': return
        try:
            with file.open('rb') as source:
                source.seek(start)
                remaining = end - start + 1
                while remaining:
                    data = source.read(min(remaining, 65536))
                    if not data: break
                    self.wfile.write(data); remaining -= len(data)
        except (BrokenPipeError, ConnectionResetError): pass

server = http.server.ThreadingHTTPServer(('127.0.0.1', a.port), Handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(a.cert, a.key)
server.socket = context.wrap_socket(server.socket, server_side=True)
server.serve_forever()
