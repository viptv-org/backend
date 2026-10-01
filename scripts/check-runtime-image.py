#!/usr/bin/env python3
"""Disposable network-none image acceptance. No browser TLS or media qualification."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import uuid


def main():
    if not __debug__:
        raise RuntimeError("Acceptance assertions must not be disabled with Python -O")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image", help="Existing local image, never pulled")
    parser.add_argument("--expect-image-id", required=True)
    parser.add_argument("--expect-revision", required=True)
    parser.add_argument("--sudo", action="store_true")
    args = parser.parse_args()
    assert re.fullmatch(r"sha256:[a-f0-9]{64}", args.expect_image_id)
    assert re.fullmatch(r"[a-f0-9]{40}", args.expect_revision)
    docker = (["sudo"] if args.sudo else []) + ["docker"]
    evidence = Path(tempfile.mkdtemp(prefix="viptv-image-acceptance-"))
    os.chmod(evidence, 0o700)
    container = None
    checks = []

    def run(arguments, data=None, check=True):
        result = subprocess.run(docker + arguments, input=data, capture_output=True, timeout=20)
        if check and result.returncode:
            raise AssertionError("Docker operation failed (private diagnostics retained)")
        return result

    def record(label):
        checks.append(label)

    def request(path, method="GET", body=None, session=None, expected=200, bearer=None):
        command = ["exec", "-i", container, "curl", "--silent", "--show-error", "--max-time", "5",
                   "--dump-header", "-", "--request", method,
                   "--header", "Origin: https://qualification.invalid"]
        if session:
            command += ["--header", "Cookie: " + "; ".join(f"{k}={v}" for k, v in session["cookies"].items()),
                        "--header", "x-csrf-token: " + session["csrf"]]
        if bearer:
            command += ["--header", "Authorization: Bearer " + bearer]
        payload = None
        if body is not None:
            command += ["--header", "Content-Type: application/json", "--data-binary", "@-"]
            payload = body if isinstance(body, bytes) else json.dumps(body).encode()
        command += ["http://127.0.0.1:8080" + path]
        raw = run(command, payload).stdout
        headers, content = raw.split(b"\r\n\r\n", 1)
        status = int(headers.splitlines()[0].split()[1])
        assert status == expected, f"Unexpected HTTP status for {method} {path}: {status}"
        header_text = headers.decode()
        if session is not None:
            for name, value in re.findall(r"(?im)^set-cookie: (viptv_\w+)=([^;]*);", header_text):
                session["cookies"][name] = value
        return header_text, content

    def api(path, method="GET", body=None, session=None, expected=200, bearer=None):
        _, content = request(path, method, body, session, expected, bearer)
        value = json.loads(content)
        if session is not None and isinstance(value, dict) and value.get("csrf_token"):
            session["csrf"] = value["csrf_token"]
        return value

    try:
        image = json.loads(run(["image", "inspect", args.image]).stdout)[0]
        assert image["Id"] == args.expect_image_id
        assert image["Config"]["Labels"]["tech.syek.viptv.backend-revision"] == args.expect_revision
        assert image["Config"]["User"] == "10001:10001"
        assert image["Config"]["Healthcheck"]["Test"]
        container = run(["run", "--detach", "--pull=never", "--network", "none", "--read-only",
                         "--cap-drop", "ALL", "--security-opt", "no-new-privileges", "--pids-limit", "128",
                         "--memory", "512m", "--memory-swap", "512m", "--cpus", "2",
                         "--tmpfs", "/data:rw,nosuid,nodev,noexec,uid=10001,gid=10001,mode=0700,size=32m",
                         "--tmpfs", "/tmp:rw,nosuid,nodev,noexec,size=16m",
                         "--env", "VIPTV_AUTH_ORIGIN=https://qualification.invalid",
                         "--env", "VIPTV_DATABASE=/data/synthetic.sqlite", args.expect_image_id]).stdout.decode().strip()
        assert re.fullmatch(r"[a-f0-9]{64}", container)
        inspection = json.loads(run(["inspect", container]).stdout)[0]
        assert inspection["HostConfig"]["NetworkMode"] == "none"
        assert inspection["HostConfig"]["ReadonlyRootfs"]
        assert inspection["HostConfig"]["CapDrop"] == ["ALL"]
        assert not inspection["Mounts"], "No host/volume mounts permitted"
        assert not inspection["HostConfig"].get("PortBindings")
        for _ in range(40):
            try:
                api("/api/health")
                break
            except (AssertionError, ValueError):
                time.sleep(0.25)
        else:
            raise AssertionError("Container did not become ready")
        identity = run(["exec", container, "sh", "-c",
                        "id -u; grep '^CapEff:' /proc/1/status; test ! -w /app; "
                        "! command -v ffmpeg; ! command -v ffprobe; "
                        "test ! -e /usr/local/bin/ffmpeg; test ! -e /usr/local/bin/ffprobe"]).stdout.decode()
        assert identity.splitlines()[0] == "10001"
        assert "CapEff:\t0000000000000000" in identity
        assert run(["exec", container, "touch", "/app/qualification-write"], check=False).returncode != 0
        record("exact image/revision; nonroot, read-only, zero capabilities, no mounts/ports, no FFmpeg")
        static_assets = []
        for route, prefix in [("/", "/assets/"), ("/tv/", "/tv/assets/")]:
            headers, html = request(route)
            assert "text/html" in headers.lower()
            assets = re.findall(r'(?:src|href)="([^"]+)"', html.decode())
            assets = [asset for asset in assets if asset.startswith(prefix)]
            assert any(asset.endswith(".js") for asset in assets)
            if route == "/":
                assert any(asset.endswith(".css") for asset in assets)
            for asset in assets:
                headers, content = request(asset)
                assert content
                if asset.endswith(".js"):
                    assert re.search(r"(?i)content-type: (application|text)/javascript", headers)
                elif asset.endswith(".css"):
                    assert "text/css" in headers.lower()
                static_assets.append(asset)
        record("dashboard/TV HTML and linked JS/CSS assets served with correct module MIME")
        sessions = []
        credentials = []
        for index in range(2):
            credentials.append({"username": "qa_" + uuid.uuid4().hex[:12], "password": uuid.uuid4().hex})
            session = {"cookies": {}, "csrf": ""}
            registered = api("/api/auth/register", "POST", credentials[-1], session)
            assert registered["profile_id"] is None and registered["profiles"] == []
            assert session["cookies"].get("viptv_session") and session["csrf"]
            profile = api("/api/profiles", "POST", {"name": f"Synthetic {index}"}, session)
            session["profile"] = str(profile["id"])
            api("/api/auth/profile", "POST", {"profile_id": profile["id"]}, session)
            sessions.append(session)
        first, second = sessions
        assert [str(item["id"]) for item in api("/api/profiles", session=first)] == [first["profile"]]
        foreign = api(f'/api/profiles/{first["profile"]}/preferences', session=second, expected=403)
        assert foreign.get("error")
        foreign_switch = api("/api/auth/profile", "POST", {"profile_id": first["profile"]}, second, expected=403)
        assert foreign_switch.get("error")
        prefs = api(f'/api/profiles/{first["profile"]}/preferences', "PUT",
                    {"audio_language": "ja", "subtitle_language": "en", "subtitles_enabled": True,
                     "subtitle_size": "normal", "subtitle_style": "system", "autoplay": False}, first)
        assert "quality" not in prefs and prefs["audio_language"] == "ja"
        assert api(f'/api/profiles/{second["profile"]}/preferences', session=second)["audio_language"] == "en"
        retired_pref = api(f'/api/profiles/{first["profile"]}/preferences', "PUT", {"quality": "2160p"}, first, 409)
        assert retired_pref["error_code"] == "client_update_required"
        record("real registration, profile/account isolation and quality-free preference persistence")
        for route in ["/api/v2/iptv/live/channels", "/api/v2/iptv/live/categories"]:
            page = api(route, session=first)
            assert page["items"] == [] and page["catalog_id"] is None
            assert page["next_cursor"] is None and page["previous_cursor"] is None
            assert "total" not in page
        assert api("/api/v2/iptv/connections", session=first)["items"] == []
        for route, body in [("/api/v2/iptv/connections", {"name": "Synthetic", "url": "https://never-fetch.invalid",
                                                      "username": "synthetic", "password": "synthetic"}),
                            ("/api/v2/addons", {"manifest_url": "https://never-fetch.invalid/manifest.json"}),
                            ("/api/addons", {"manifest_url": "https://never-fetch.invalid/manifest.json"})]:
            denied = api(route, "POST", body, first, 503)
            assert denied["error_code"] == "secret_store_not_configured"
            assert "never-fetch" not in json.dumps(denied)
        assert api("/api/v2/iptv/connections", session=first)["items"] == []
        assert api("/api/v2/addons", session=first)["items"] == []
        record("empty lazy catalog/default; no-keyring writes refuse before fetch or storage")
        retired = ["playback", "streams", "live", "guide", "providers", "account-pools", "lineup",
                   "live-policy", "guides", "automation", "stream-health", "activity", "service-health",
                   "status", "matches", "setup"]
        for method in ["GET", "POST", "PUT", "PATCH", "DELETE"]:
            for route in [f"/api/{name}" for name in retired] + ["/api/automation/nested", "/media/retired"]:
                denied = api(route, method, b"{invalid-json", first, 409)
                assert denied["error_code"] == "client_update_required"
        assert not run(["exec", container, "sh", "-c", "cat /proc/1/task/*/children"]).stdout.strip()
        record("retired namespaces refuse all exercised methods; no media child processes")
        old_cookie = dict(first["cookies"])
        api("/api/auth/refresh", "POST", {}, first)
        assert first["cookies"]["viptv_session"] != old_cookie["viptv_session"]
        stale = {"cookies": old_cookie, "csrf": first["csrf"]}
        api("/api/profiles", session=stale, expected=401)
        api("/api/auth/logout", "POST", {}, first)
        api("/api/profiles", session=first, expected=401)
        login = {"cookies": {}, "csrf": ""}
        api("/api/auth/login", "POST", credentials[0], login)
        api("/api/profiles", session=login)
        api("/api/auth/sessions", "DELETE", session=login)
        api("/api/profiles", session=login, expected=401)
        native = api("/api/auth/device/login", "POST", credentials[1])
        native_refresh = api("/api/auth/device/refresh", "POST", {"refresh_token": native["refresh_token"]})
        assert native_refresh["access_token"] != native["access_token"]
        api("/api/profiles", bearer=native["access_token"], expected=401)
        api("/api/profiles", bearer=native_refresh["access_token"])
        api("/api/auth/logout", "POST", {}, bearer=native_refresh["access_token"])
        api("/api/profiles", bearer=native_refresh["access_token"], expected=401)
        record("browser/native login, refresh rotation, stale-token rejection, logout/session revocation")
        for _ in range(45):
            status = json.loads(run(["inspect", container]).stdout)[0]["State"]["Health"]["Status"]
            if status == "healthy":
                break
            time.sleep(1)
        assert status == "healthy"
        record("actual configured Docker healthcheck reports healthy")
        summary = {"image_id": image["Id"], "source_revision": args.expect_revision,
                   "checks": checks, "static_assets": static_assets,
                   "runtime": {"uid": 10001, "network": "none", "read_only": True,
                               "mounts": [], "published_ports": [], "effective_capabilities": 0,
                               "healthcheck": status},
                   "boundary": "Synthetic network-none container HTTP only; no browser TLS, production or media qualification"}
        (evidence / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        os.chmod(evidence / "summary.json", 0o600)
        print(f"PASS: {len(checks)} acceptance groups; private evidence {evidence}")
    except Exception as error:
        print(f"FAIL: {type(error).__name__}; private diagnostics {evidence}")
        raise
    finally:
        if container and re.fullmatch(r"[a-f0-9]{64}", container):
            logs = run(["logs", container], check=False)
            (evidence / "container.log").write_bytes(logs.stdout + logs.stderr)
            os.chmod(evidence / "container.log", 0o600)
            run(["container", "rm", "--force", container])


if __name__ == "__main__":
    main()
