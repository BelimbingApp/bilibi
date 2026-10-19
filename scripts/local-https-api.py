#!/usr/bin/env python3
"""Plan a local origin; only the HTTPS owner's checkout may register with Caddy."""
import hashlib
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
ROOT = Path(__file__).resolve().parent.parent
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


def listeners():
    in_use = set()
    for line in subprocess.run(["ss", "-H", "-ltn"], check=True, capture_output=True, text=True).stdout.splitlines():
        address = line.split()[3] if len(line.split()) > 3 else ""
        match = re.search(r":(\d+)$", address)
        if match:
            in_use.add(int(match.group(1)))
    return in_use


def owns_route(route):
    """Keep the claim in Caddy, including while the owner's app is stopped.

    Adopt a pre-claim route only if its live backend runs in this checkout or
    its loaded certificate matches this checkout's certificate. Unknown legacy
    ownership fails closed; a free port never grants ownership of a host.
    """
    if route is None:
        return not os.environ.get("BILIBI_DEV_PORT")
    claims = [value["@id"] for value in walk(route) if value.get("@id", "").startswith(OWNER_PREFIX)]
    if claims:
        return claims == [OWNER_ID]
    port = route_port(route)
    if port in listeners():
        result = subprocess.run(["ss", "-H", "-ltnp", f"sport = :{port}"],
                                check=True, capture_output=True, text=True)
        for pid in re.findall(r"\bpid=(\d+)", result.stdout):
            try:
                if Path(f"/proc/{pid}/cwd").resolve(strict=True) == ROOT:
                    return True
            except OSError:
                pass
        return False
    certificate = ROOT / ".local" / "https" / f"{HOST}.pem"
    loaded = get_id(CERT_ID)
    return bool(certificate.is_file() and loaded and loaded.get("certificate") == certificate.read_text())


def choose_port(servers, requested_port, own_route=None):
    configured_port = route_port(own_route) if own_route else None
    if configured_port and not os.environ.get("BILIBI_DEV_PORT"):
        return configured_port
    in_use = listeners()
    for server in servers.values():
        for route in caddy_routes(server):
            if route != own_route:
                in_use.update(port for value in walk(route) if (port := route_port(value)))
    if os.environ.get("BILIBI_DEV_PORT") and requested_port in in_use:
        raise RuntimeError(f"BILIBI_DEV_PORT {requested_port} is already in use or reserved by Caddy.")
    port = requested_port
    while port in in_use:
        port += 1
    if port > 65535:
        raise RuntimeError("No free backend port is available for Bilibi.")
    return port


def plan(requested_port):
    try:
        servers = request("GET", "/config/apps/http/servers") or {}
        route = get_id(ROUTE_ID)
        owner = owns_route(route)
        # Do not claim a host already registered by a different launcher.
        if route is None and any(HOST in matcher.get("host", [])
                                 for server in servers.values() for entry in caddy_routes(server)
                                 for matcher in entry.get("match", [])):
            owner = False
    except (ApiError, RuntimeError):
        servers, route, owner = {}, None, False
    port = choose_port(servers, requested_port, route if owner else None)
    print("https" if owner else "http")
    print(HOST if owner else "127.0.0.1")
    print(port)


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


def certificate_files(host, certificate, key):
    ca_root = subprocess.check_output(["mkcert", "-CAROOT"], text=True).strip()
    checks = [
        ["openssl", "x509", "-in", str(certificate), "-checkend", "2592000", "-noout"],
        ["openssl", "verify", "-CAfile", str(Path(ca_root) / "rootCA.pem"),
         "-verify_hostname", host, str(certificate)],
    ]
    if not (certificate.is_file() and key.is_file()) or any(
        subprocess.run(check, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode
        for check in checks
    ):
        old_mask = os.umask(0o077)
        try:
            subprocess.run(["mkcert", "-cert-file", str(certificate), "-key-file", str(key), host],
                           check=True, stdout=sys.stderr)
        finally:
            os.umask(old_mask)
    return certificate.read_text(), key.read_text()


def main():
    global HOST, ROUTE_ID, TLS_POLICY_ID, CERT_ID, CERT_TAG, ERROR_ROUTE_ID, OWNER_PREFIX, OWNER_ID
    planning = len(sys.argv) == 4 and sys.argv[1] in ("--plan", "--http")
    host = sys.argv[2] if planning else sys.argv[1] if len(sys.argv) == 5 else ""
    if not re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*(?:\.[a-z0-9]+(?:-[a-z0-9]+)*)*\.localhost", host):
        raise RuntimeError("BILIBI_DEV_HOST must be a hostname under .localhost.")
    HOST = host
    # Preserve the existing default IDs; a lab host must never patch that route.
    if host != "bilibi.localhost":
        suffix = "-" + host
        ROUTE_ID += suffix
        TLS_POLICY_ID += suffix
        CERT_ID += suffix
        CERT_TAG += suffix
        ERROR_ROUTE_ID += suffix
    OWNER_PREFIX = ROUTE_ID + "-owner-"
    OWNER_ID = OWNER_PREFIX + hashlib.sha256(str(ROOT).encode()).hexdigest()
    requested_port = int(sys.argv[3] if planning else sys.argv[2])
    if not 1024 <= requested_port <= 65535:
        raise RuntimeError("BILIBI_DEV_PORT must be 1024..65535.")
    if planning:
        if sys.argv[1] == "--http":
            print(f"http\n127.0.0.1\n{choose_port({}, requested_port)}")
        else:
            plan(requested_port)
        return
    if len(sys.argv) != 5:
        raise RuntimeError("usage: local-https-api.py HOST PORT CERTIFICATE KEY")
    # local-https.sh serializes registration. Recheck even after --plan, and
    # before any TLS or HTTP write, so a port override cannot steal the host.
    own_route = get_id(ROUTE_ID)
    if not owns_route(own_route):
        raise RuntimeError(f"This checkout does not own https://{host}; use its local HTTP address.")
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
    if own_route is None:
        for existing_server in config.values():
            for route in caddy_routes(existing_server):
                if any(host in matcher.get("host", []) for matcher in route.get("match", [])):
                    raise RuntimeError(f"Caddy already has a route for {host} without Bilibi's launch ID.")
    port = choose_port(config, requested_port, own_route)
    certificate, private_key = certificate_files(host, Path(sys.argv[3]), Path(sys.argv[4]))

    certificate_config = {
        "@id": CERT_ID,
        "certificate": certificate,
        "key": private_key,
        "tags": [CERT_TAG],
    }
    if "load_pem" in tls_config:
        upsert_id(
            CERT_ID,
            certificate_config,
            "/config/apps/tls/certificates/load_pem",
            certificate_config,
        )
    elif tls_config:
        request("PUT", "/config/apps/tls/certificates/load_pem", [certificate_config])
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
            if "tls_connection_policies" in server:
                request("POST", server_path + "/tls_connection_policies", tls_policy)
            else:
                request("PUT", server_path + "/tls_connection_policies", [tls_policy])
        else:
            request("PUT", f"{server_path}/tls_connection_policies/{default_policy}", tls_policy)

    route_config = {
        "@id": ROUTE_ID,
        "match": [{"host": [host]}],
        "handle": [{
            "@id": OWNER_ID,
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
