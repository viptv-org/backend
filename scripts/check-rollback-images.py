#!/usr/bin/env python3
"""Current-data rollback between two exact engine-free images; synthetic only."""
import argparse
import base64
import contextlib
import hashlib
import json
import os
from pathlib import Path
import re
import sqlite3
import subprocess
import tempfile
import time
import uuid


def main():
    if not __debug__ or os.geteuid() != 0:
        raise RuntimeError("Run this synthetic local-volume fixture with sudo; assertions required")
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("prior_image_id")
    p.add_argument("prior_revision")
    p.add_argument("current_image_id")
    p.add_argument("current_revision")
    args = p.parse_args()
    for value in [args.prior_image_id, args.current_image_id]:
        assert re.fullmatch(r"sha256:[0-9a-f]{64}", value)
    for value in [args.prior_revision, args.current_revision]:
        assert re.fullmatch(r"[0-9a-f]{40}", value)
    assert args.current_image_id != args.prior_image_id
    os.umask(0o077)
    evidence = Path(tempfile.mkdtemp(prefix="viptv-rollback-images-"))
    identifier = uuid.uuid4().hex
    volume_name = "viptv-rollback-fixture-" + identifier
    containers = set()
    container = None
    volume = None
    mountpoint = None
    checks = []
    keyring = json.dumps({"active": "fixture", "keys": {"fixture": base64.b64encode(os.urandom(32)).decode()}})
    username = "rollback_" + uuid.uuid4().hex[:12]
    password = uuid.uuid4().hex
    session = {"cookies": {}, "csrf": ""}

    def docker(arguments, data=None, check=True, timeout=30):
        try:
            result = subprocess.run(["docker", *arguments], input=data, capture_output=True, timeout=timeout)
        except subprocess.TimeoutExpired:
            raise RuntimeError("Bounded Docker operation timed out; no command secrets printed") from None
        if check and result.returncode:
            (evidence / "docker-error.log").write_bytes(result.stdout + result.stderr)
            raise RuntimeError("Docker operation failed; private diagnostics retained")
        return result

    def scoped_volume():
        v = json.loads(docker(["volume", "inspect", volume_name]).stdout)[0]
        assert v["Name"] == volume_name and v["Driver"] == "local"
        assert v["Labels"].get("tech.syek.viptv.test-owner") == identifier
        root = Path(v["Mountpoint"])
        assert root.is_absolute() and root.name == "_data" and root.parent.name == volume_name
        return root

    def database():
        assert container is None, "All normal writers must be stopped before offline SQL"
        assert scoped_volume() == mountpoint
        path = mountpoint / "synthetic.sqlite"
        assert path.is_file() and not path.is_symlink()
        return contextlib.closing(sqlite3.connect(path))

    def snapshot():
        tables = ["auth_accounts", "profiles", "profile_owners", "auth_profiles", "auth_sessions",
                  "favorites", "progress", "queue_hidden", "viewing_settings", "playback_preferences",
                  "providers", "provider_ownership", "provider_credentials_v2", "provider_live",
                  "provider_vod", "provider_matches", "provider_live_generations", "provider_live_categories_v2",
                  "account_media_settings", "addons", "addon_credentials_v2", "retired_features_v2"]
        result = {}
        with database() as db:
            assert db.execute("PRAGMA quick_check").fetchone()[0] == "ok"
            assert db.execute("PRAGMA foreign_key_check").fetchall() == []
            for table in tables:
                rows = db.execute(f'SELECT * FROM "{table}" ORDER BY rowid').fetchall()
                encoded = json.dumps(rows, default=lambda value: {"blob": base64.b64encode(value).decode()}, separators=(",", ":")).encode()
                result[table] = {"rows": len(rows), "sha256": hashlib.sha256(encoded).hexdigest()}
        return result

    def api(path, method="GET", body=None, expected=200, bearer=None):
        assert container in containers
        command = ["exec", "-i", container, "curl", "--silent", "--show-error", "--max-time", "5",
                   "--dump-header", "-", "--request", method, "--header", "Origin: https://rollback.invalid"]
        if bearer:
            command += ["--header", "Authorization: Bearer " + bearer]
        elif session["cookies"]:
            command += ["--header", "Cookie: " + "; ".join(f"{key}={value}" for key, value in session["cookies"].items()),
                        "--header", "x-csrf-token: " + session["csrf"]]
        payload = None
        if body is not None:
            command += ["--header", "Content-Type: application/json", "--data-binary", "@-"]
            payload = json.dumps(body).encode()
        command += ["http://127.0.0.1:8080" + path]
        headers, content = docker(command, payload).stdout.split(b"\r\n\r\n", 1)
        status = int(headers.splitlines()[0].split()[1])
        assert status == expected, f"Unexpected fixture HTTP status for {method} {path}: {status}"
        for key, value in re.findall(r"(?im)^set-cookie: (viptv_\w+)=([^;]*);", headers.decode()):
            session["cookies"][key] = value
        result = json.loads(content)
        if isinstance(result, dict) and result.get("csrf_token"):
            session["csrf"] = result["csrf_token"]
        return result

    def start(image):
        nonlocal container
        assert container is None
        container = docker(["run", "--detach", "--pull=never", "--network", "none", "--read-only",
                            "--cap-drop", "ALL", "--security-opt", "no-new-privileges", "--pids-limit", "128",
                            "--memory", "512m", "--memory-swap", "512m", "--cpus", "2",
                            "--mount", f"type=volume,src={volume_name},dst=/data",
                            "--tmpfs", "/tmp:rw,nosuid,nodev,noexec,size=16m",
                            "--env", "VIPTV_DATABASE=/data/synthetic.sqlite",
                            "--env", "VIPTV_AUTH_ORIGIN=https://rollback.invalid",
                            "--env", "VIPTV_SECRETS_KEYRING=" + keyring, image]).stdout.decode().strip()
        assert re.fullmatch(r"[0-9a-f]{64}", container)
        containers.add(container)
        for _ in range(40):
            try:
                api("/api/health")
                break
            except (AssertionError, ValueError, RuntimeError):
                time.sleep(0.25)
        else:
            raise RuntimeError("Fixture server did not become ready")
        info = json.loads(docker(["inspect", container]).stdout)[0]
        assert info["HostConfig"]["NetworkMode"] == "none" and info["HostConfig"]["ReadonlyRootfs"]
        assert info["HostConfig"]["CapDrop"] == ["ALL"] and not info["HostConfig"].get("PortBindings")
        assert len(info["Mounts"]) == 1 and info["Mounts"][0]["Name"] == volume_name

    def stop():
        nonlocal container
        assert container in containers
        docker(["stop", "--time", "5", container], timeout=12)
        assert not json.loads(docker(["inspect", container]).stdout)[0]["State"]["Running"]
        (evidence / f"server-{len(checks)}.log").write_bytes(docker(["logs", container]).stdout)
        docker(["container", "rm", container])
        containers.remove(container)
        container = None

    def offline(arguments):
        assert container is None and scoped_volume() == mountpoint
        result = docker(["run", "--rm", "--pull=never", "--network", "none", "--read-only",
                         "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
                         "--mount", f"type=volume,src={volume_name},dst=/data",
                         "--tmpfs", "/tmp:rw,nosuid,nodev,noexec,size=16m",
                         "--env", "VIPTV_SECRETS_KEYRING=" + keyring,
                         "--entrypoint", "/usr/local/bin/provider-owners", args.current_image_id, *arguments], timeout=30)
        return json.loads(result.stdout)

    try:
        for image_id, revision in [(args.prior_image_id, args.prior_revision), (args.current_image_id, args.current_revision)]:
            info = json.loads(docker(["image", "inspect", image_id]).stdout)[0]
            assert info["Id"] == image_id and info["Config"]["Labels"]["tech.syek.viptv.backend-revision"] == revision
            assert info["Config"]["User"] == "10001:10001"
        volume = docker(["volume", "create", "--driver", "local", "--label", "tech.syek.viptv.test-owner=" + identifier, volume_name]).stdout.decode().strip()
        assert volume == volume_name
        mountpoint = scoped_volume()
        # Setup-only helper can change the exact newly created mount's owner.
        # The normal backend subsequently runs nonroot with no capabilities.
        docker(["run", "--rm", "--pull=never", "--network", "none", "--read-only", "--user", "0:0",
                "--cap-drop", "ALL", "--cap-add", "CHOWN", "--cap-add", "FOWNER",
                "--mount", f"type=volume,src={volume_name},dst=/data",
                "--entrypoint", "sh", args.current_image_id, "-c", "chown 10001:10001 /data && chmod 700 /data"])
        start(args.current_image_id)
        api("/api/auth/register", "POST", {"username": username, "password": password, "name": "Synthetic rollback member"})
        profile = api("/api/profiles", "POST", {"name": "Preserved profile"})["id"]
        api("/api/auth/profile", "POST", {"profile_id": profile})
        me = api("/api/auth/me")
        account = int(me.get("account_id") or me.get("id") or me.get("account", {}).get("id"))
        native = api("/api/auth/device/login", "POST", {"username": username, "password": password})
        access = native["access_token"]
        api("/api/auth/profile", "POST", {"profile_id": profile}, bearer=access)
        stop()
        preservation = mountpoint / "preservation"
        preservation.mkdir(mode=0o700)
        os.chown(preservation, 10001, 10001)
        with database() as db:
            db.execute("PRAGMA foreign_keys=ON")
            db.execute("INSERT INTO providers(id,name,url,username,password) VALUES(31,'Fixture','http://11.255.255.1/private','fixture-user','fixture-password')")
            db.execute("INSERT INTO provider_ownership VALUES(31,?)", (account,))
            db.execute("INSERT INTO provider_live(id,provider_id,stream_id,name,category_id,category,ordinal) VALUES('iptv:31:7',31,'7','Raw channel','news','News',0),('iptv:31:8',31,'8','Second channel','news','News',1)")
            db.execute("INSERT INTO provider_live_generations VALUES(31,9)")
            db.execute("INSERT INTO provider_live_categories_v2 VALUES(31,'news','News',0)")
            db.execute("INSERT INTO account_media_settings VALUES(?,31)", (account,))
            db.execute("INSERT INTO provider_vod(id,provider_id,stream_id,kind,name,normalized,extension) VALUES('vod:31:8',31,'8','movie','Fixture movie','fixture movie','mp4')")
            db.execute("INSERT INTO provider_matches VALUES('vod:31:8','tt1234567','movie')")
            db.execute("INSERT INTO favorites(profile_id,id,type,name) VALUES(?,'family:historical','live','Archived reference')", (profile,))
            db.execute("INSERT INTO progress(profile_id,id,type,name,position,duration,updated_at,title_id) VALUES(?,'tt1234567','movie','Fixture movie',123.5,900,1,'tt1234567')", (profile,))
            db.execute("INSERT INTO queue_hidden VALUES(?,'movie','tt8888888')", (profile,))
            archived_preferences = {"audio_language":"en","subtitle_language":"en","subtitles_enabled":False,
                                    "subtitle_size":"normal","subtitle_style":"system","autoplay":True,"quality":"1080p"}
            db.execute("INSERT INTO playback_preferences(profile_id,value) VALUES(?,?) ON CONFLICT(profile_id) DO UPDATE SET value=excluded.value", (profile, json.dumps(archived_preferences)))
            db.execute("CREATE TABLE family_channels(id TEXT PRIMARY KEY,data TEXT NOT NULL)")
            db.execute("INSERT INTO family_channels VALUES('family:historical','{\"fixture\":true}')")
            db.commit()
            addon_ids = [row[0] for row in db.execute("SELECT id FROM addons WHERE account_id<=0")]
        owner_file = preservation / "addon-owners.json"
        owner_file.write_text(json.dumps({str(addon): account for addon in addon_ids}))
        os.chown(owner_file, 10001, 10001)
        revision = args.current_revision
        offline(["apply-addons", "/data/synthetic.sqlite", "/data/preservation/addon-owners.json", "/data/preservation/owners.sqlite", "/data/preservation/owners.json", revision, "--confirm-ownership"])
        offline(["encrypt", "/data/synthetic.sqlite", "/data/preservation/providers.sqlite", "/data/preservation/providers.json", revision, "--confirm-encryption"])
        offline(["encrypt-addons", "/data/synthetic.sqlite", "/data/preservation/addons.sqlite", "/data/preservation/addons.json", revision, "--confirm-encryption"])
        offline(["retire", "/data/synthetic.sqlite", "/data/preservation/retirement.sqlite", "/data/preservation/retirement.json", revision, "--confirm-retirement"])
        before = snapshot()
        checks.append("explicit ownership/encryption/backup-first retirement on synthetic closed DB")
        # Start the selected prior engine-free release, then newer current code,
        # and finally roll back without restoring any older database snapshot.
        for phase, image in [("prior", args.prior_image_id), ("current", args.current_image_id), ("rollback", args.prior_image_id)]:
            start(image)
            assert [int(p["id"]) for p in api("/api/profiles", bearer=access)] == [int(profile)]
            prefs = api(f"/api/profiles/{profile}/preferences", bearer=access)
            assert "quality" not in prefs
            first = api("/api/v2/iptv/live/channels?limit=1", bearer=access)
            assert first["catalog_id"] == 31 and first["generation"] == 9 and first["items"][0]["id"] == "iptv:31:7"
            second = api("/api/v2/iptv/live/channels?limit=1&cursor=" + first["next_cursor"], bearer=access)
            assert second["items"][0]["id"] == "iptv:31:8"
            assert api("/api/v2/iptv/live/channels?limit=1&cursor=" + second["previous_cursor"], bearer=access)["items"] == first["items"]
            addons = api("/api/v2/addons", bearer=access)["items"]
            assert addons and all(row["credentials_encrypted"] and not row.get("configuration_error") for row in addons)
            # Default management remains browser/account-only; device sessions
            # browse it implicitly but cannot inspect/edit account settings.
            assert api("/api/v2/iptv/live-default")["catalog_id"] == 31
            source = api("/api/v2/iptv/live/iptv:31:7/source", "POST", {}, bearer=access)["source"]
            assert source["id"] and source["source"] == "iptv:31"
            assert "fixture-user" not in json.dumps(source) and "fixture-password" not in json.dumps(source)
            assert api("/api/v2/iptv/live/family:historical/source", "POST", {}, bearer=access, expected=404)["error_code"] == "source_not_found"
            assert api("/media/old", bearer=access, expected=409)["error_code"] == "client_update_required"
            if phase == "current":
                api(f"/api/profiles/{profile}/progress", "PUT", {"id":"tt1234567","type":"movie","name":"Fixture movie","position":456.75,"duration":900}, bearer=access)
                api(f"/api/profiles/{profile}/progress", "PUT", {"id":"tt7654321","type":"movie","name":"Later history","position":99,"duration":1000}, bearer=access)
                api(f"/api/profiles/{profile}/preferences", "PUT", {"audio_language":"ja","subtitle_language":"en","subtitles_enabled":True,"subtitle_size":"normal","subtitle_style":"system","autoplay":False}, bearer=access)
            if phase == "rollback":
                assert api(f"/api/profiles/{profile}/preferences", bearer=access)["audio_language"] == "ja"
                history = api(f"/api/profiles/{profile}/progress/page", bearer=access)["items"]
                assert {row["id"]: row["position"] for row in history} == {"tt1234567":456.75,"tt7654321":99}
            stop()
            after = snapshot()
            if phase == "current":
                # Autoplay is intentionally mirrored in viewing_settings by the
                # existing preference writer; all other persisted rows are fixed.
                allowed = {"progress", "playback_preferences", "viewing_settings"}
                changed = [table for table in before if before[table] != after[table] and table not in allowed]
                assert not changed, "Unexpected preservation change in tables: " + ", ".join(changed)
                with database() as db:
                    assert db.execute("SELECT autoplay FROM viewing_settings WHERE profile_id=?", (profile,)).fetchone()[0] == 0
                before = after
            else:
                assert before == after, "Image switch changed preserved data (no private row dump)"
            with database() as db:
                assert not db.execute("SELECT 1 FROM sqlite_master WHERE name='family_channels'").fetchall()
                assert json.loads(db.execute("SELECT value FROM playback_preferences WHERE profile_id=?", (profile,)).fetchone()[0])["quality"] == "1080p"
            checks.append(f"{phase} image: retained auth/profile/source/default/crypto/history; no retired table resurrection")
        summary = {"prior_image_id":args.prior_image_id,"prior_revision":args.prior_revision,
                   "current_image_id":args.current_image_id,"current_revision":args.current_revision,"checks":checks,
                   "preserved_tables":before,"boundary":"Synthetic current-data engine-free image rollback via container HTTP; not legacy rollback, browser/media/hardware or production"}
        (evidence / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    finally:
        failures = []
        for ident in list(containers):
            logs = docker(["logs", ident], check=False)
            (evidence / f"failed-{ident[:12]}.log").write_bytes(logs.stdout + logs.stderr)
            result = docker(["container", "rm", "--force", ident], check=False)
            if result.returncode:
                failures.append("owned container")
        if volume:
            # Validate the exact fresh volume label again; never target a caller volume.
            scoped_volume()
            if docker(["volume", "rm", volume_name], check=False).returncode:
                failures.append("owned volume")
        if failures:
            raise RuntimeError("Fixture cleanup failed; inspect private evidence")
    print(f"PASS: {len(checks)} current-data image rollback groups; owned synthetic volume/data removed; private evidence {evidence}")


if __name__ == "__main__":
    main()
