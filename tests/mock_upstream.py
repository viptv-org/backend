#!/usr/bin/env python3
"""Deterministic local Xtream/Stremio fixtures. Never use as a public service."""
import argparse
import json
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, unquote, urlsplit


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass  # Provider URLs contain credentials; do not log request targets.

    def do_GET(self):
        parsed = urlsplit(self.path)
        path = unquote(parsed.path)
        base = getattr(self.server, 'public_base', None) or f'http://127.0.0.1:{self.server.server_port}'
        meta = {'id': 'tt1234567', 'type': 'movie', 'name': 'Fixture Movie',
                'year': '2024', 'releaseInfo': '2024', 'description': 'Local integration fixture.'}
        series = {'id': 'tt7654321', 'type': 'series', 'name': 'Fixture Series',
                  'releaseInfo': '2024', 'videos': [{'id': 'tt7654321:1:1', 'title': 'Pilot', 'season': 1, 'episode': 1}]}
        if path.endswith('/manifest.json'):
            prefix = path.split('/')[1]
            return self.json({'id': f'fixture.{prefix}', 'version': '1.0.0', 'name': f'Fixture {prefix}',
                              'description': 'Deterministic integration fixture', 'types': ['movie', 'series'],
                              'resources': ['catalog', 'meta', 'stream'], 'idPrefixes': ['tt'],
                              'catalogs': [{'type': 'movie', 'id': 'top', 'name': 'Fixture movies',
                                            'extra': [{'name': 'search', 'isRequired': False}, {'name': 'skip'}]},
                                           {'type': 'series', 'id': 'top', 'name': 'Fixture series'}]})
        if '/catalog/' in path:
            return self.json({'metas': [series if '/series/' in path else meta]})
        if '/meta/' in path:
            return self.json({'meta': series if '/series/' in path else meta})
        if '/stream/' in path:
            if path.startswith('/slow/'):
                time.sleep(1.2)
            return self.json({'streams': [{'name': 'Fixture HTTP · 1080p',
                                          'title': 'Fixture.Movie.2024.mp4\n🇮🇹 / 🇺🇸 · H264 AAC',
                                          'description': 'Italian + English + English Commentary\nMulti-audio integration fixture',
                                          'languages': ['ita', 'eng'],
                                          'behaviorHints': {'filename': 'Fixture.Movie.2024.mp4',
                                                            'videoSize': self.server.media.stat().st_size},
                                          'url': f'{base}/media/fixture.mp4'}]})
        if path == '/player_api.php':
            q = parse_qs(parsed.query)
            if q.get('username') != ['fixture'] or q.get('password') != ['fixture-secret']:
                return self.json({'user_info': {'auth': 0}}, 401)
            action = q.get('action', [''])[0]
            if action == 'get_series_info' and self.server.series_delay:
                time.sleep(self.server.series_delay)
            now = int(time.time())
            responses = {
                '': {'user_info': {'auth': 1, 'status': 'Active', 'max_connections': '3'},
                     'server_info': {'url': urlsplit(base).hostname,
                                     'port': str(urlsplit(base).port or (443 if urlsplit(base).scheme == 'https' else 80)),
                                     'server_protocol': urlsplit(base).scheme}},
                'get_live_categories': [{'category_id': '1', 'category_name': 'Fixture live'}],
                'get_vod_categories': [{'category_id': '2', 'category_name': 'Hidden movies'}],
                'get_series_categories': [{'category_id': '3', 'category_name': 'Hidden series'}],
                'get_live_streams': [{'stream_id': 101, 'name': 'Fixture Channel', 'category_id': '1',
                                      'stream_icon': '', 'epg_channel_id': 'fixture.channel', 'stream_type': 'live'}],
                'get_vod_streams': [{'stream_id': 201, 'name': 'Fixture Movie (2024)', 'year': '2024',
                                     'category_id': '2', 'container_extension': 'mp4', 'stream_icon': '',
                                     'imdb_id': 'tt1234567', 'stream_type': 'movie'}],
                'get_series': [{'series_id': 301, 'name': 'Fixture Series', 'year': '2024',
                                'category_id': '3', 'cover': '', 'imdb_id': 'tt7654321'}],
                'get_vod_info': {'info': {'name': 'Fixture Movie', 'releasedate': '2024-01-01',
                                         'imdb_id': 'tt1234567'},
                                 'movie_data': {'stream_id': 201, 'container_extension': 'mp4', 'name': 'Fixture Movie'}},
                'get_series_info': {'info': {'name': 'Fixture Series', 'imdb_id': 'tt7654321'},
                                    'episodes': {'1': [{'id': '401', 'episode_num': 1, 'season': 1,
                                                        'title': 'Pilot', 'container_extension': 'mp4', 'info': {}}]}},
                'get_short_epg': {'epg_listings': [{'id': '1', 'title': 'Rml4dHVyZSBOZXdz',
                                                   'description': 'VGVzdCBwcm9ncmFt',
                                                   'start_timestamp': str(now - 900), 'stop_timestamp': str(now + 900)}]},
                'get_simple_data_table': {'epg_listings': []},
            }
            return self.json(responses.get(action, []))
        if path.startswith(('/media/', '/movie/', '/series/', '/live/')):
            return self.media()
        self.json({'error': 'fixture route not found'}, 404)

    def json(self, value, status=200):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def media(self):
        file = self.server.media
        if not file.is_file():
            return self.json({'error': 'Generate fixture.mp4 first'}, 404)
        total = file.stat().st_size
        start, end = 0, total - 1
        partial = False
        request_range = self.headers.get('Range', '')
        if request_range.startswith('bytes='):
            try:
                left, right = request_range[6:].split('-', 1)
                if left:
                    start = int(left)
                    end = min(int(right), end) if right else end
                else:
                    start = max(0, total - int(right))
                if start < 0 or start > end:
                    raise ValueError()
                partial = True
            except ValueError:
                self.send_response(416)
                self.send_header('Content-Range', f'bytes */{total}')
                self.end_headers()
                return
        self.send_response(206 if partial else 200)
        self.send_header('Content-Type', 'video/mp4')
        self.send_header('Accept-Ranges', 'bytes')
        self.send_header('Content-Length', str(end - start + 1))
        if partial:
            self.send_header('Content-Range', f'bytes {start}-{end}/{total}')
        self.end_headers()
        try:
            with file.open('rb') as stream:
                stream.seek(start)
                remaining = end - start + 1
                while remaining:
                    chunk = stream.read(min(65536, remaining))
                    if not chunk:
                        break
                    self.wfile.write(chunk)
                    remaining -= len(chunk)
        except (BrokenPipeError, ConnectionResetError):
            pass


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--port', type=int, default=19090)
    parser.add_argument('--media', type=Path, default=Path('artifacts/fixture.mp4'))
    parser.add_argument('--series-delay', type=float, default=0, help='Delay lazy series detail responses for isolation tests')
    parser.add_argument('--bind', default='127.0.0.1', help='Listen address; default remains loopback')
    parser.add_argument('--public-base', help='Advertised HTTP(S) origin, e.g. http://fixtures:19090')
    args = parser.parse_args()
    if args.public_base:
        origin = urlsplit(args.public_base)
        if origin.scheme not in ('http', 'https') or not origin.hostname or origin.path not in ('', '/') or origin.query or origin.fragment or origin.username or origin.password:
            parser.error('--public-base must be an HTTP(S) origin without credentials')
        try:
            origin.port
        except ValueError:
            parser.error('--public-base has an invalid port')
    server = ThreadingHTTPServer((args.bind, args.port), Handler)
    server.media = args.media.resolve()
    server.series_delay = max(0, args.series_delay)
    server.public_base = args.public_base.rstrip('/') if args.public_base else None
    print(f'Mock upstream listening on http://{args.bind}:{server.server_port}', flush=True)
    server.serve_forever()
