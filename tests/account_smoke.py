#!/usr/bin/env python3
"""Disposable real-HTTP acceptance for public accounts and paired Roku devices."""
import json
import os
from http.cookies import SimpleCookie
from urllib.error import HTTPError
from urllib.request import Request, urlopen

BASE = os.environ.get("VIPTV_TEST_URL", "http://127.0.0.1:18080").rstrip("/")
ORIGIN = os.environ.get("VIPTV_TEST_ORIGIN", BASE.replace("http://", "https://", 1))
USERNAME = os.environ.get("VIPTV_TEST_PUBLIC_USERNAME", "acceptance-member")
PASSWORD = os.environ.get("VIPTV_TEST_PUBLIC_PASSWORD", "account-acceptance-member-password")


class Browser:
    def __init__(self):
        self.cookies = {}
        self.csrf = ""

    def request(self, path, method="GET", body=None, expected=200):
        headers = {"Accept": "application/json", "User-Agent": "VIPTV-Account-Smoke/1.6"}
        if self.cookies:
            headers["Cookie"] = "; ".join(f"{key}={value}" for key, value in self.cookies.items())
        if method not in ("GET", "HEAD"):
            headers["Origin"] = ORIGIN
            if self.csrf:
                headers["X-CSRF-Token"] = self.csrf
        data = None
        if body is not None:
            headers["Content-Type"] = "application/json"
            data = json.dumps(body).encode()
        request = Request(BASE + "/api" + path, data=data, headers=headers, method=method)
        try:
            response = urlopen(request, timeout=30)
        except HTTPError as error:
            response = error
        payload = response.read()
        result = json.loads(payload) if payload else None
        for value in response.headers.get_all("Set-Cookie", []):
            cookie = SimpleCookie(); cookie.load(value)
            for key, morsel in cookie.items():
                if morsel.value:
                    self.cookies[key] = morsel.value
                else:
                    self.cookies.pop(key, None)
        if isinstance(result, dict) and isinstance(result.get("csrf_token"), str):
            self.csrf = result["csrf_token"]
        if response.status != expected:
            raise AssertionError(f"{method} {path}: expected {expected}, got {response.status}: {result}")
        return result


def bearer(path, token, method="GET", body=None, expected=200):
    headers = {"Accept": "application/json", "Authorization": "Bearer " + token,
               "User-Agent": "VIPTV-Account-Smoke/1.6"}
    data = None
    if body is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(body).encode()
    request = Request(BASE + "/api" + path, data=data, headers=headers, method=method)
    try:
        response = urlopen(request, timeout=30)
    except HTTPError as error:
        response = error
    payload = response.read()
    result = json.loads(payload) if payload else None
    if response.status != expected:
        raise AssertionError(f"{method} {path}: expected {expected}, got {response.status}: {result}")
    return result


def check(condition, message):
    if not condition:
        raise AssertionError(message)
    print("PASS", message, flush=True)


def main():
    browser = Browser()
    status = browser.request("/auth/status")
    check(status.get("registration_enabled") is True, "public registration is enabled")
    registered = browser.request("/auth/register", "POST", {
        "username": USERNAME, "name": "Acceptance Member", "password": PASSWORD
    })
    check(bool(browser.cookies) and bool(browser.csrf), "registration establishes a protected browser session")
    check(bool(registered.get("recovery_code") or registered.get("recovery_codes")),
          "registration returns one-time recovery material")
    me = browser.request("/auth/me")
    check(me["account"]["role"] == "member", "public registration never creates an administrator")
    check(browser.request("/profiles") == [], "new account starts with zero profiles")

    code = Browser().request("/device/code", "POST", {"device_name": "Acceptance Roku"})
    check(all(code.get(key) for key in ("device_code", "user_code", "verification_uri_complete", "qr_uri")),
          "device pairing returns canonical code, deep link, and QR fields")
    browser.request("/device/approve", "POST", {"user_code": code["user_code"]})
    device = Browser().request("/device/token", "POST", {"device_code": code["device_code"]})
    access = device["access_token"]
    device_me = bearer("/auth/me", access)
    check(device_me.get("profile_id") is None and bearer("/profiles", access) == [],
          "paired device exchange succeeds before profile creation")

    check(device_me.get("can_create_profile") is True and device_me.get("can_manage_profiles") is True,
          "paired device exposes native profile management capabilities")
    profile = bearer("/profiles", access, "POST", {"name": "Kid", "avatar_style": "critters"})
    check(profile.get("avatar_url", "").startswith("https://api.dicebear.com/10.x/critters/png?"),
          "paired device creates an account-owned remote-avatar profile")
    bearer("/auth/profile", access, "POST", {"profile_id": profile["id"]})
    check(bearer("/auth/me", access).get("profile_id") == str(profile["id"]),
          "device explicitly selects its own profile")
    bearer("/providers", access, expected=403)
    check(True, "paired device cannot administer the server")

    profiles = browser.request("/profiles")
    check([str(item["id"]) for item in profiles] == [str(profile["id"])],
          "browser and device share only their account-owned profile")
    devices = browser.request("/devices")
    row = next(item for item in devices if item["device_name"] == "Acceptance Roku")
    browser.request(f"/devices/{row['id']}", "DELETE")
    bearer("/auth/me", access, expected=401)
    bearer("/profiles", "obsolete-static-bearer-is-never-authorized", expected=401)
    check(True, "revocation is immediate and arbitrary static bearers never authenticate")


if __name__ == "__main__":
    main()
