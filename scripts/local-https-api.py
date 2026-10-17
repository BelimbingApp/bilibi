#!/usr/bin/env python3
"""Register Bilibi's route and in-memory certificate in the running Caddy config."""
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import Request, urlopen


API = os.environ.get("BILIBI_CADDY_ADMIN_URL", "http://localhost:2019").rstrip("/")
ROUTE_ID = "bilibi-local-route"
TLS_POLICY_ID = "bilibi-local-tls-policy"
CERT_ID = "bilibi-local-certificate"
CERT_TAG = "bilibi-local-certificate"
ERROR_ROUTE_ID = "bilibi-local-error-route"
WAITING_PAGE = Path(__file__).resolve().with_name("waiting-page.html")
# What reverse_proxy reports when the backend refuses, resets or times out.
BACKEND_DOWN = "[502, 503, 504]"


class ApiError(RuntimeError):
    def __init__(self, status, message):
        super().__init__(message)
        self.status = status


def request(method, path, body=None):
    data = None if body is None else json.dumps(body).encode()
    headers = {} if data is None else {"Content-Type": "application/json"}
    req = Request(API + path, data=data, headers=headers, method=method)
    try:
        with urlopen(req, timeout=8) as response:
            raw = response.read()
    except HTTPError as error:
        detail = error.read().decode(errors="replace").strip()
        raise ApiError(error.code, detail or error.reason) from error
    except URLError as error:
        raise RuntimeError(f"Caddy admin API unavailable at {API}: {error.reason}") from error
    if not raw:
        return None
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return raw.decode(errors="replace")


def get_id(item_id):
    try:
        return request("GET", "/id/" + quote(item_id, safe=""))
    except ApiError as error:
        if error.status == 404:
            return None
        raise


def upsert_id(item_id, value, create_path, create_value):
    encoded = quote(item_id, safe="")
    if get_id(item_id) is None:
        request("POST", create_path, create_value)
    else:
        request("PATCH", "/id/" + encoded, value)


def walk(value):
    if isinstance(value, dict):
        yield value
        for child in value.values():
            yield from walk(child)
    elif isinstance(value, list):
        for child in value:
            yield from walk(child)


def route_port(route):
    for value in walk(route):
        if value.get("handler") == "reverse_proxy":
            for upstream in value.get("upstreams", []):
                dial = upstream.get("dial", "")
                match = re.search(r":(\d+)$", dial)
                if match:
                    return int(match.group(1))
    return None


def caddy_routes(server):
    return server.get("routes", [])


def choose_port(server, host, requested_port):
    own_route = get_id(ROUTE_ID)
    configured_port = route_port(own_route) if own_route else None
    if os.environ.get("BILIBI_DEV_PORT"):
        return requested_port
    if configured_port:
        return configured_port

    in_use = set()
    for line in subprocess.run(["ss", "-H", "-ltn"], check=True, capture_output=True, text=True).stdout.splitlines():
        address = line.split()[3] if len(line.split()) > 3 else ""
        match = re.search(r":(\d+)$", address)
        if match:
            in_use.add(int(match.group(1)))
    for route in caddy_routes(server):
        if route.get("@id") != ROUTE_ID:
            used = route_port(route)
            if used:
                in_use.add(used)
    port = requested_port
    while port in in_use:
        port += 1
    if port > 65535:
        raise RuntimeError("No free backend port is available for Bilibi.")
    return port


def waiting_route(host):
    """Caddy's error route: a branded page while the proxy is up and Bilibi is not.

    The page rides in the config itself because Caddy may not be able to read
    this checkout. static_response expands {placeholders} in its body, so every
    brace in the page is escaped. X-Bilibi-Waiting lets the page tell its own
    answer from the app's when it polls for the backend to return.
    """
    body = WAITING_PAGE.read_text().replace("{", "\\{")
    return {
        "@id": ERROR_ROUTE_ID,
        "match": [{
            "host": [host],
            "expression": "{http.error.status_code} in " + BACKEND_DOWN,
        }],
        "handle": [{
            "handler": "static_response",
            "status_code": 503,
            "headers": {
                "Content-Type": ["text/html; charset=utf-8"],
                "Cache-Control": ["no-store"],
                "Retry-After": ["3"],
                "X-Bilibi-Waiting": ["1"],
            },
            "body": body,
        }],
        "terminal": True,
    }


def install_waiting_route(server_path, server, host):
    route = waiting_route(host)
    if get_id(ERROR_ROUTE_ID) is not None:
        request("PATCH", "/id/" + quote(ERROR_ROUTE_ID, safe=""), route)
    elif server.get("errors"):
        request("POST", server_path + "/errors/routes", route)
    else:
        request("PUT", server_path + "/errors", {"routes": [route]})


def allowed_clients():
    clients = ["127.0.0.1", "::1"]
    if shutil.which("wslinfo"):
        mode = subprocess.run(["wslinfo", "--networking-mode"], capture_output=True, text=True)
        if mode.returncode == 0 and mode.stdout.strip() == "nat" and shutil.which("ip"):
            route = subprocess.run(["ip", "-4", "route", "show", "default"], capture_output=True, text=True)
            match = re.search(r"\bvia ([0-9.]+)", route.stdout)
            if match:
                clients.append(match.group(1))
    return clients


def report_missing_certificate_files(config, servers):
    certificates = config.get("load_files", [])
    hosts_by_tag = {}
    for server in servers.values():
        for policy in server.get("tls_connection_policies", []):
            hosts = (policy.get("match") or {}).get("sni", [])
            tags = (policy.get("certificate_selection") or {}).get("any_tag", [])
            for tag in tags:
                hosts_by_tag.setdefault(tag, set()).update(hosts)

    missing = []
    for certificate in certificates:
        absent = [
            path for path in (certificate.get("certificate"), certificate.get("key"))
            if path and not Path(path).is_file()
        ]
        if absent:
            hosts = sorted({
                host
                for tag in certificate.get("tags", [])
                for host in hosts_by_tag.get(tag, set())
            })
            label = ", ".join(hosts) if hosts else ", ".join(certificate.get("tags", [])) or "unknown site"
            missing.append(f"{label}: {', '.join(absent)}")
    if missing:
        print("Caddy config references missing certificate files; HTTPS may not reload: " + "; ".join(missing), file=sys.stderr)


def main():
    if len(sys.argv) != 5:
        raise RuntimeError("usage: local-https-api.py HOST PORT CERTIFICATE KEY")
    host, requested_port = sys.argv[1], int(sys.argv[2])
    certificate = Path(sys.argv[3]).read_text()
    private_key = Path(sys.argv[4]).read_text()
    try:
        tls_config = request("GET", "/config/apps/tls/certificates") or {}
    except ApiError as error:
        # A TLS app without loaded certificates has no such key yet.
        if error.status != 400:
            raise
        tls_config = {}
    config = request("GET", "/config/apps/http/servers")
    report_missing_certificate_files(tls_config, config)
    candidates = [
        (name, server)
        for name, server in config.items()
        if ":443" in server.get("listen", [])
    ]
    if not candidates:
        raise RuntimeError("Caddy has no HTTPS server listening on :443.")
    server_name, server = candidates[0]
    server_path = "/config/apps/http/servers/" + quote(server_name, safe="")
    port = choose_port(server, host, requested_port)

    certificate_config = {
        "@id": CERT_ID,
        "certificate": certificate,
        "key": private_key,
        "tags": [CERT_TAG],
    }
    if tls_config:
        upsert_id(
            CERT_ID,
            certificate_config,
            "/config/apps/tls/certificates/load_pem",
            [certificate_config],
        )
    else:
        # No certificates yet, and possibly no TLS app at all (a bare :443 server).
        try:
            request("PUT", "/config/apps/tls/certificates", {"load_pem": [certificate_config]})
        except ApiError as error:
            if error.status != 400:
                raise
            request("PUT", "/config/apps/tls", {"certificates": {"load_pem": [certificate_config]}})

    tls_policy = {
        "@id": TLS_POLICY_ID,
        "match": {"sni": [host]},
        "certificate_selection": {"any_tag": [CERT_TAG]},
    }
    if get_id(TLS_POLICY_ID) is not None:
        request("PATCH", "/id/" + quote(TLS_POLICY_ID, safe=""), tls_policy)
    else:
        policies = server.get("tls_connection_policies", [])
        default_policy = next((i for i, policy in enumerate(policies) if not policy.get("match")), None)
        if default_policy is None:
            request("POST", server_path + "/tls_connection_policies", [tls_policy])
        else:
            request("PUT", f"{server_path}/tls_connection_policies/{default_policy}", tls_policy)

    own_route = get_id(ROUTE_ID)
    if own_route is None:
        for route in caddy_routes(server):
            hosts = [host_name for matcher in route.get("match", []) for host_name in matcher.get("host", [])]
            if host in hosts:
                raise RuntimeError(f"Caddy already has a route for {host} without Bilibi's launch ID.")

    route_config = {
        "@id": ROUTE_ID,
        "match": [{"host": [host]}],
        "handle": [{
            "handler": "subroute",
            "routes": [
                {
                    "match": [{"not": [{"remote_ip": {"ranges": allowed_clients()}}]}],
                    "handle": [{
                        "handler": "static_response",
                        "body": "Local development only",
                        "status_code": 403,
                    }],
                },
                {
                    "handle": [{
                        "handler": "reverse_proxy",
                        "upstreams": [{"dial": f"127.0.0.1:{port}"}],
                    }],
                },
            ],
        }],
        "terminal": True,
    }
    if own_route is None:
        if "routes" in server:
            request("POST", server_path + "/routes", route_config)
        else:
            request("PUT", server_path + "/routes", [route_config])
    else:
        request("PATCH", "/id/" + quote(ROUTE_ID, safe=""), route_config)
    install_waiting_route(server_path, server, host)
    print(port)


if __name__ == "__main__":
    try:
        main()
    except (ApiError, OSError, RuntimeError, ValueError) as error:
        print(str(error).splitlines()[0], file=sys.stderr)
        raise SystemExit(1)
