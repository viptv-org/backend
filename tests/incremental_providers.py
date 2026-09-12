#!/usr/bin/env python3
"""Prove early IPTV results survive an independently slow provider.

Start mock_upstream.py on19090 and a second instance --port19091 --series-delay3.
Use a disposable backend and the owner browser credentials required by smoke.py.
"""
import os
import time
from smoke import request, check, authenticate


def main():
    authenticate()
    profiles = request('/api/profiles')
    profile = next((p for p in profiles if p.get('setup_complete') is True), None)
    if profile is None:
        profile = request('/api/profiles', 'POST', {
            'name': 'Provider isolation profile', 'avatar_style': 'critters'})
    request('/api/auth/profile', 'POST', {'profile_id': profile['id']})
    providers = []
    try:
        upstreams = [('Fast provider', os.environ.get('VIPTV_TEST_FAST_UPSTREAM', 'http://127.0.0.1:19090')),
                     ('Slow provider', os.environ.get('VIPTV_TEST_SLOW_UPSTREAM', 'http://127.0.0.1:19091'))]
        for name, upstream in upstreams:
            provider = request('/api/providers', 'POST', {'name': name, 'url': upstream.rstrip('/'),
                                                          'username': 'fixture', 'password': 'fixture-secret'})
            providers.append(provider)
            request(f'/api/providers/{provider["id"]}/sync', 'POST', {})
        started = time.monotonic()
        job = request('/api/streams', 'POST', {'type': 'series', 'id': 'tt7654321:1:1',
                                               'name': 'Fixture Series', 'year': 2024, 'season': 1, 'episode': 1})
        cursor, fast_at, slow_at, fast_partial = 0, None, None, False
        all_events = []
        while time.monotonic() - started < 15:
            batch = request(f'/api/streams/{job["id"]}?after={cursor}')
            all_events.extend(batch['events'])
            for event in batch['events']:
                cursor = max(cursor, event['seq'])
                if event.get('streams') and event.get('source') == f'iptv:{providers[0]["id"]}':
                    fast_at = fast_at or time.monotonic() - started
                    fast_partial |= not batch['done']
                if event.get('streams') and event.get('source') == f'iptv:{providers[1]["id"]}':
                    slow_at = slow_at or time.monotonic() - started
            if batch['done']:
                break
            time.sleep(.05)
        check(fast_at is not None, 'fast IPTV provider emits its own playable event')
        check(slow_at is not None, 'slow IPTV provider eventually emits its own playable event')
        check(fast_partial, 'fast IPTV event arrives while discovery is still running')
        check(fast_at < 2.5 and slow_at - fast_at >= 1.5,
              f'providers are isolated (fast={fast_at:.2f}s, slow={slow_at:.2f}s)')
        check(len({e['seq'] for e in all_events}) == len(all_events), 'provider event cursor is append-only')
        print('Provider isolation smoke passed.', flush=True)
    finally:
        for provider in providers:
            request(f'/api/providers/{provider["id"]}', 'DELETE')


if __name__ == '__main__':
    main()
