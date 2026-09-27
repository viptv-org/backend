#!/usr/bin/env python3
"""Compare two FFmpeg executables on controlled HTTPS sources, without deployment."""
import argparse
import json
import pathlib
import statistics
import subprocess
import tempfile
import time

p = argparse.ArgumentParser()
p.add_argument('--baseline', required=True)
p.add_argument('--candidate', required=True)
p.add_argument('--source-base', required=True)
p.add_argument('--device', default='/dev/dri/renderD128')
p.add_argument('--ca-file')
p.add_argument('--repeats', type=int, default=5)
p.add_argument('--output', required=True)
args = p.parse_args()
if not args.source_base.startswith('https://'):
    p.error('The controlled fixture origin must use HTTPS')

def version(binary):
    return subprocess.check_output([binary, '-version'], text=True).splitlines()[0]

def run(binary, profile, source):
    with tempfile.TemporaryDirectory(prefix='viptv-encode-bench-') as directory:
        root = pathlib.Path(directory)
        command = [binary, '-v', 'error', '-nostdin', '-y']
        if profile == 'qsv':
            command += ['-init_hw_device', f'qsv=viptv,child_device={args.device}', '-filter_hw_device', 'viptv',
                        '-hwaccel', 'qsv', '-hwaccel_output_format', 'qsv', '-c:v', 'hevc_qsv']
        if args.ca_file:
            command += ['-tls_verify', '1', '-ca_file', args.ca_file]
        command += ['-i', args.source_base.rstrip('/') + '/' + source, '-t', '10', '-map', '0:v:0', '-map', '0:a:0', '-sn']
        if profile in ('copy', 'audio'):
            command += ['-c:v', 'copy']
        elif profile == 'qsv':
            command += ['-vf', 'scale_qsv=w=1280:h=720:format=nv12', '-c:v', 'h264_qsv', '-preset', 'veryfast',
                        '-profile:v', 'main', '-level:v', '4.0', '-b:v', '4000k', '-maxrate', '5000k', '-bufsize', '10000k',
                        '-look_ahead', '0', '-async_depth', '1', '-bf', '0', '-fpsmax', '30', '-g', '60', '-forced_idr', '1',
                        '-force_key_frames', 'expr:gte(t,if(eq(n_forced,0),0,1+(n_forced-1)*2))']
        else:
            command += ['-vf', 'scale=1280:720', '-c:v', 'libx264', '-preset', 'ultrafast', '-crf', '23', '-pix_fmt', 'yuv420p',
                        '-g', '60', '-keyint_min', '60', '-sc_threshold', '0', '-force_key_frames', 'expr:gte(t,if(eq(n_forced,0),0,1+(n_forced-1)*2))']
        command += ['-c:a', 'copy'] if profile == 'copy' else ['-c:a', 'aac', '-b:a', '128k', '-ac', '2', '-ar', '48000']
        command += ['-f', 'hls', '-hls_init_time', '1', '-hls_time', '2', '-hls_list_size', '0', '-hls_flags', 'temp_file',
                    '-hls_segment_filename', str(root / 'segment-%03d.ts'), str(root / 'index.m3u8')]
        start = time.monotonic()
        with open(root / 'stderr', 'wb') as error:
            process = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=error)
            first = None
            while process.poll() is None:
                if first is None and (root / 'segment-000.ts').exists():
                    first = time.monotonic() - start
                if time.monotonic() - start > 45:
                    process.kill()
                    process.wait()
                    break
                time.sleep(.01)
        elapsed = time.monotonic() - start
        if process.returncode or first is None:
            return {'ok': False, 'exit': process.returncode, 'elapsed': elapsed}
        durations = [float(line[8:].split(',')[0]) for line in (root / 'index.m3u8').read_text().splitlines() if line.startswith('#EXTINF:')]
        # A separate real decode verifies that a fast producer did not merely emit corrupt bytes.
        check = subprocess.run([binary, '-v', 'error', '-i', str(root / 'index.m3u8'), '-f', 'null', '-'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
        return {'ok': check.returncode == 0, 'first_segment_ms': round(first * 1000, 1), 'speed': round(sum(durations) / elapsed, 3), 'seconds': sum(durations)}

report = {'versions': {'baseline': version(args.baseline), 'candidate': version(args.candidate)}, 'profiles': {}}
adopt = True
for profile, source in [('copy', 'h264-aac.mkv'), ('audio', 'h264-ac3.mkv'), ('software', 'hevc-main10.mkv'), ('qsv', 'hevc-main10.mkv')]:
    records = {}
    for name, binary in [('baseline', args.baseline), ('candidate', args.candidate)]:
        values = [run(binary, profile, source) for _ in range(args.repeats)]
        valid = [value for value in values if value['ok']]
        records[name] = {'runs': values, 'compatible': len(valid) == len(values)}
        if valid:
            times = sorted(value['first_segment_ms'] for value in valid)
            records[name].update(p95_first_segment_ms=times[min(len(times) - 1, int(.95 * len(times)))], minimum_speed=min(value['speed'] for value in valid))
        print(profile, name, json.dumps({k: v for k, v in records[name].items() if k != 'runs'}), flush=True)
    candidate, baseline = records['candidate'], records['baseline']
    passed = candidate['compatible'] and baseline['compatible'] and candidate.get('minimum_speed', 0) >= 1.25 and candidate.get('p95_first_segment_ms', float('inf')) <= baseline.get('p95_first_segment_ms', 0) * 1.10
    adopt &= passed
    report['profiles'][profile] = {**records, 'passed': passed}
    report['adopt_candidate'] = adopt
    pathlib.Path(args.output).write_text(json.dumps(report, indent=2))
print('adopt_candidate:', adopt)
