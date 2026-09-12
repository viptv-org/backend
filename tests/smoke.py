#!/usr/bin/env python3
"""Real HTTP smoke tests using an owner browser session and disposable data.

Set VIPTV_TEST_USERNAME, VIPTV_TEST_PASSWORD, VIPTV_TEST_ORIGIN, and optionally
VIPTV_TEST_URL. Never pass account credentials on a command line.
"""
import json
import math
import array
import re
import os
import time
import subprocess
import tempfile
from http.cookies import SimpleCookie
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request, urlopen
from urllib.parse import quote, urljoin

BASE = os.environ.get('VIPTV_TEST_URL', 'http://127.0.0.1:18080').rstrip('/')
ORIGIN = os.environ.get('VIPTV_TEST_ORIGIN', BASE.replace('http://', 'https://', 1))
USERNAME = os.environ['VIPTV_TEST_USERNAME']
PASSWORD = os.environ['VIPTV_TEST_PASSWORD']
UPSTREAM = os.environ.get('VIPTV_TEST_UPSTREAM', 'http://127.0.0.1:19090').rstrip('/')
COOKIES = {}
CSRF = ''


def request(path, method='GET', body=None, auth=True, raw=False):
    global CSRF
    headers = {'Accept': 'application/json', 'User-Agent': 'VIPTV-Disposable-Smoke/1.6'}
    if auth:
        if not COOKIES:
            raise AssertionError('Owner browser session is not authenticated')
        headers['Cookie'] = '; '.join(f'{key}={value}' for key, value in COOKIES.items())
    if method not in ('GET', 'HEAD'):
        headers['Origin'] = ORIGIN
        if auth and CSRF:
            headers['X-CSRF-Token'] = CSRF
    data = None
    if body is not None:
        headers['Content-Type'] = 'application/json'
        data = json.dumps(body).encode()
    req = Request(BASE + path, data=data, headers=headers, method=method)
    with urlopen(req, timeout=60) as response:
        payload = response.read()
        for value in response.headers.get_all('Set-Cookie', []):
            parsed = SimpleCookie(); parsed.load(value)
            for key, morsel in parsed.items():
                if morsel.value:
                    COOKIES[key] = morsel.value
                else:
                    COOKIES.pop(key, None)
        if not raw and payload:
            result = json.loads(payload)
            if isinstance(result, dict) and isinstance(result.get('csrf_token'), str):
                CSRF = result['csrf_token']
            return result
        return payload if raw else None


def authenticate():
    request('/api/auth/login', 'POST', {'username': USERNAME, 'password': PASSWORD}, auth=False)
    check(bool(COOKIES) and bool(CSRF), 'owner login establishes Secure cookie and CSRF session')
    me = request('/api/auth/me')
    check(me.get('account', {}).get('role') == 'owner', 'smoke credentials belong to an owner account')


def check(condition, message):
    if not condition:
        raise AssertionError(message)
    print('PASS', message, flush=True)


def first_segment(playback):
    playlist = request(playback['url'], auth=False, raw=True).decode()
    check('#EXTM3U' in playlist, 'scoped playback URL serves HLS playlist')
    segment = next(line for line in playlist.splitlines() if line and not line.startswith('#'))
    segment_url = playback['url'].rsplit('/', 1)[0] + '/' + segment
    data = request(segment_url, auth=False, raw=True)
    check(len(data) > 188 and data[0] == 0x47, 'HLS delivers MPEG-TS media')
    return data


def check_tone(data, expected_frequency, message):
    ffmpeg = os.environ.get('VIPTV_TEST_FFMPEG')
    if not ffmpeg:
        return
    with tempfile.TemporaryDirectory(prefix='viptv-audio-') as directory:
        path = Path(directory) / 'segment.ts'
        path.write_bytes(data)
        pcm = subprocess.check_output([ffmpeg, '-v', 'error', '-i', str(path),
            '-map', '0:a:0', '-t', '1', '-ac', '1', '-ar', '8000', '-f', 's16le', 'pipe:1'],
            stderr=subprocess.PIPE, timeout=15)
    samples = array.array('h')
    samples.frombytes(pcm)
    if __import__('sys').byteorder != 'little':
        samples.byteswap()
    check(len(samples) >= 4000, 'decoded audio contains enough samples for signal verification')
    def power(frequency):
        omega = 2 * math.pi * frequency / 8000
        real = sum(value * math.cos(omega * i) for i, value in enumerate(samples))
        imaginary = sum(value * math.sin(omega * i) for i, value in enumerate(samples))
        return real * real + imaginary * imaginary
    expected = power(expected_frequency)
    alternatives = [power(f) for f in [440, 660, 880, 1320] if f != expected_frequency]
    check(expected > max(alternatives) * 20, message)


def main():
    check(request('/api/health', auth=False)['status'] == 'ok', 'public health')
    authenticate()
    try:
        request('/api/providers', auth=False)
    except HTTPError as exc:
        check(exc.code == 401, 'administrative endpoints require bearer authentication')
    else:
        raise AssertionError('Unauthenticated provider API was accessible')
    created = []
    playback = None
    try:
        profile = request('/api/profiles', 'POST', {'name': 'Smoke profile', 'avatar_style': 'pixel-art'})
        check(bool(profile.get('id')) and profile.get('setup_complete', profile.get('presentation_complete')) is True,
              'create account-owned profile with remote avatar presentation')
        request('/api/auth/profile', 'POST', {'profile_id': profile['id']})
        provider = request('/api/providers', 'POST', {'name': 'Smoke fixture', 'url': UPSTREAM,
                                                       'username': 'fixture', 'password': 'fixture-secret'})
        created.append(('providers', provider['id']))
        providers = request('/api/providers')
        check('fixture-secret' not in json.dumps(providers), 'provider list redacts password')
        request(f'/api/providers/{provider["id"]}/sync', 'POST', {})
        channels = request('/api/live?limit=10')['channels']
        check(any(c['name'] == 'Fixture Channel' for c in channels), 'Xtream live synchronization')
        categories = request('/api/live/categories?limit=100')
        category = next(c for c in categories['categories'] if c['name'] == 'Fixture live')
        category_channels = request('/api/live?category=' + quote(category['id'], safe=''))
        check(category['count'] == category_channels['total'] == 1,
              'complete live categories link to exact counted channel groups')
        guide = request('/api/guide/' + channels[0]['id'])
        check(bool(guide['programs']), 'Xtream guide retrieval')
        for name in ['fast', 'slow']:
            addon = request('/api/addons', 'POST', {'manifest_url': f'{UPSTREAM}/{name}/manifest.json'})
            created.append(('addons', addon['id']))
        catalogs = request('/api/catalogs')
        check(any(c['id'] == 'top' for c in catalogs), 'configured addon catalogs')
        favorite = {'id': 'tt1234567', 'type': 'movie', 'name': 'Fixture Movie', 'poster': ''}
        request(f'/api/profiles/{profile["id"]}/favorites', 'PUT', favorite)
        favorites = request(f'/api/profiles/{profile["id"]}/favorites')
        check(any(f['id'] == favorite['id'] for f in favorites), 'profile favorites persisted')
        request(f'/api/profiles/{profile["id"]}/progress', 'PUT', dict(favorite, position=7, duration=20))
        progress = request(f'/api/profiles/{profile["id"]}/progress')
        check(any(p['position'] == 7 for p in progress), 'continue watching persisted')
        job = request('/api/streams', 'POST', {'type': 'movie', 'id': 'tt1234567', 'name': 'Fixture Movie', 'year': 2024})
        cursor, events, partial = 0, [], False
        deadline = time.monotonic() + 35
        while time.monotonic() < deadline:
            batch = request(f'/api/streams/{job["id"]}?after={cursor}')
            events.extend(batch['events'])
            if batch['events']:
                cursor = max(e['seq'] for e in events)
                partial |= not batch['done']
            if batch['done']:
                break
            time.sleep(.1)  # Test client polling, not agent background-job polling.
        else:
            raise AssertionError('Stream discovery did not complete within deadline')
        check(partial, 'results arrive before all source providers finish')
        streams = [s for e in events for s in e.get('streams', [])]
        check(bool(streams), 'stream discovery produces registered candidates')
        check(any(s.get('source', '').startswith('iptv') for s in streams), 'matched Xtream VOD appears beside addon streams')
        check(any(s.get('source', '').startswith('addon:') for s in streams), 'addon streams appear beside matched Xtream VOD')
        addon_stream = next(s for s in streams if s.get('source', '').startswith('addon:'))
        check('🇮🇹 / 🇺🇸' in addon_stream['title']
              and 'English Commentary' in addon_stream['description']
              and addon_stream['reported_languages'] == ['ita', 'eng']
              and addon_stream['filename'] == 'Fixture.Movie.2024.mp4'
              and addon_stream['size_bytes'] > 0,
              'Stremio title, separate description, language reports and file details survive discovery')
        check(addon_stream['audio_language_status'] == 'unverified'
              and not any(k in addon_stream for k in ['url', 'headers', 'behaviorHints']),
              'reported language is not falsely verified and private fetch context remains hidden')
        check('fixture-secret' not in json.dumps(events), 'stream discovery redacts IPTV credentials')
        check(len({e['seq'] for e in events}) == len(events), 'cursor polling does not repeat events')
        episode_job = request('/api/streams', 'POST', {'type': 'series', 'id': 'tt7654321:1:1',
                                                       'name': 'Fixture Series', 'year': 2024, 'season': 1, 'episode': 1})
        episode_events, episode_cursor = [], 0
        deadline = time.monotonic() + 35
        while time.monotonic() < deadline:
            batch = request(f'/api/streams/{episode_job["id"]}?after={episode_cursor}')
            episode_events.extend(batch['events'])
            if episode_events:
                episode_cursor = max(e['seq'] for e in episode_events)
            if batch['done']:
                break
            time.sleep(.1)
        else:
            raise AssertionError('Episode discovery exceeded deadline')
        check(any(e.get('source', '').startswith('iptv') and e.get('streams') for e in episode_events),
              'lazy Xtream episode lookup matches series metadata')
        if os.environ.get('VIPTV_TEST_PLAYBACK', '1') == '1':
            playback = request('/api/playback', 'POST', {'stream_id': streams[0]['id'], 'force_transcode': True, 'position': 5,
                                                         'capabilities': {'max_width': 320, 'max_height': 180,
                                                                          'h264': True, 'hevc': False, 'aac': True}})
            check(playback['mode'] == 'transcode' and playback['video_mode'] == 'encode'
                  and playback['audio_mode'] == 'encode',
                  'forced FFmpeg transcode encodes video and selected audio')
            check(playback['selected_audio']['input_index'] == 2
                  and playback['selected_audio']['language'] == 'eng',
                  'English main audio wins over a non-English declared default')
            check(all(t['selectable'] for t in playback['audio_tracks'])
                  and any(t['input_index'] == 1 and t['language'] == 'ita' for t in playback['audio_tracks']),
                  'all bounded audio tracks remain selectable with informational language labels')
            check(playback['position'] == 5, 'resume response retains original timeline offset')
            check(19 <= playback.get('duration', 0) <= 21, 'resume response reports full source duration rather than rolling window')
            data = first_segment(playback)
            check_tone(data, 880, 'decoded selected audio follows the English main track')
            probe = os.environ.get('VIPTV_TEST_FFPROBE')
            if probe:
                with tempfile.TemporaryDirectory(prefix='viptv-probe-') as directory:
                    path = Path(directory) / 'segment.ts'
                    path.write_bytes(data)
                    output = subprocess.check_output([probe, '-v', 'error', '-show_streams', '-of', 'json', str(path)], timeout=15)
                    tracks = json.loads(output)['streams']
                    video = next(t for t in tracks if t['codec_type'] == 'video')
                    audio = next(t for t in tracks if t['codec_type'] == 'audio')
                    check(video['codec_name'] == 'h264' and video['width'] <= 320 and video['height'] <= 180,
                          'decoded output respects requested H264 resolution cap')
                    check(video['pix_fmt'] == 'yuv420p' and audio['codec_name'] == 'aac' and audio['channels'] == 2,
                          'output uses old-Roku-compatible pixel format and AAC stereo')
            request('/api/playback/' + playback['id'] + '/heartbeat', 'POST', {})
            request('/api/playback/' + playback['id'], 'DELETE')
            playback = None
            check(request('/api/status')['active_sessions'] == 0, 'session shutdown releases playback capacity')
            playback = request('/api/playback', 'POST', {
                'stream_id': streams[0]['id'], 'audio_track_index': 2, 'position': 0,
                'capabilities': {'max_width': 1280, 'max_height': 720,
                                 'h264': True, 'hevc': False, 'aac': True}})
            check(playback['mode'] == 'remux' and playback['video_mode'] == 'copy'
                  and playback['audio_mode'] == 'copy',
                  'compatible video and audio are fully transmuxed without encoding')
            check(len(first_segment(playback)) > 188, 'transmuxed HLS segment is readable')
            request('/api/playback/' + playback['id'], 'DELETE')
            playback = None
            for index, frequency, label in [(3, 660, 'explicit English commentary track selection'),
                                             (2, 1320, 'seek reaches later original-source audio timeline')]:
                playback = request('/api/playback', 'POST', {
                    'stream_id': streams[0]['id'], 'audio_track_index': index,
                    'position': 12, 'force_transcode': True,
                    'capabilities': {'max_width': 320, 'max_height': 180,
                                     'h264': True, 'hevc': False, 'aac': True}})
                check(playback['selected_audio']['input_index'] == index,
                      'client-selected input audio index is honored')
                check(playback['position'] == 12 and 19 <= playback['duration'] <= 21,
                      'track restart preserves absolute position and complete source duration')
                check_tone(first_segment(playback), frequency, label)
                request('/api/playback/' + playback['id'], 'DELETE')
                playback = None
            playback = request('/api/playback', 'POST', {
                'stream_id': streams[0]['id'], 'audio_track_index': 2,
                'subtitle_track_index': 4, 'position': 12, 'force_transcode': True,
                'capabilities': {'max_width': 320, 'max_height': 180,
                                 'h264': True, 'hevc': False, 'aac': True}})
            check(playback['subtitles_supported'] and playback['selected_subtitle']['input_index'] == 4,
                  'client can select the actual embedded English subtitle track')
            master = request(playback['url'], auth=False, raw=True).decode()
            check('#EXT-X-STREAM-INF:' in master and 'TYPE=SUBTITLES' in master,
                  'selected captions return a complete playable HLS master')
            subtitle_uri = re.search(r'URI="([^"]+)"', master).group(1)
            subtitle_path = urljoin(playback['url'], subtitle_uri)
            capability_directory = playback['url'].rsplit('/', 1)[0] + '/'
            check(subtitle_path.startswith(capability_directory), 'subtitle rendition retains the media capability')
            caption_deadline = time.monotonic() + 10
            caption_text = ''
            while time.monotonic() < caption_deadline:
                subtitle_playlist = request(subtitle_path, auth=False, raw=True).decode()
                for line in subtitle_playlist.splitlines():
                    if line and not line.startswith('#'):
                        vtt_path = urljoin(subtitle_path, line)
                        check(vtt_path.startswith(capability_directory), 'VTT segment remains within the same capability')
                        with urlopen(BASE + vtt_path, timeout=10) as response:
                            check(response.headers.get_content_type() == 'text/vtt', 'actual runtime serves WebVTT MIME')
                            caption_text += response.read(1048576).decode()
                if 'Later English caption' in caption_text:
                    break
                time.sleep(.2)
            check('WEBVTT' in caption_text and 'Later English caption' in caption_text,
                  'actual image FFmpeg emits the later source caption after seek')
            check('X-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:0' in caption_text,
                  'caption and video output share an explicit zero-based clock')
            request('/api/playback/' + playback['id'], 'DELETE')
            playback = None
            playback = request('/api/playback', 'POST', {
                'stream_id': streams[0]['id'], 'audio_track_index': 1,
                'capabilities': {'max_width': 1280, 'max_height': 720,
                                 'h264': True, 'hevc': False, 'aac': True}})
            check(playback['selected_audio']['input_index'] == 1,
                  'explicit non-English track selection is accepted')
            request('/api/playback/' + playback['id'], 'DELETE')
            playback = None
            check(request('/api/status')['active_sessions'] == 0,
                  'track switches and seek leave no active session')
        request(f'/api/profiles/{profile["id"]}/favorites/movie/tt1234567', 'DELETE')
        check(not request(f'/api/profiles/{profile["id"]}/favorites'), 'favorite removal')
        print('All smoke assertions passed.', flush=True)
    finally:
        if playback:
            request('/api/playback/' + playback['id'], 'DELETE')
        for collection, identifier in reversed(created):
            request(f'/api/{collection}/{identifier}', 'DELETE')


if __name__ == '__main__':
    main()
