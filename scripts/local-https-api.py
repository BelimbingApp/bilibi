#!/usr/bin/env python3
"""Plan a local origin; only a host's owning checkout may register it with Caddy.

Local launcher plumbing, with no Bilimbi module counterpart.

Every Caddy object this script writes (route, TLS policy, certificate, waiting
route) carries an @id keyed by host, so several checkouts register side by
side and one never replaces or steals another's. A checkout owns a host when
the host's route carries its owner claim (a hash of the checkout path).
"""
import hashlib
import importlib.util
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


spec = importlib.util.spec_from_file_location("dev_env", Path(__file__).with_name("dev-env.py"))
dev_env = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dev_env)

API = os.environ.get("BILIBI_CADDY_ADMIN_URL", "http://localhost:2019").rstrip("/")
# One @id per kind and host: <base>-<host>. Before per-host IDs, bilibi.localhost
# used the bare bases; find() still locates them and the owner's next
# registration rewrites each in place under its per-host @id.
BASES = {
    "route": "bilibi-local-route",
    "tls_policy": "bilibi-local-tls-policy",
    "certificate": "bilibi-local-certificate",
    "error_route": "bilibi-local-error-route",
}
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


def find(item_id):
    """(existing @id, value) for a per-host ID, falling back to the bare ID it replaces."""
    value = get_id(item_id)
    if value is None and item_id in LEGACY_IDS:
        item_id = LEGACY_IDS[item_id]
        value = get_id(item_id)
    return (item_id if value is not None else None), value


def upsert_id(item_id, value, create_path, create_value):
    existing, _ = find(item_id)
    if existing is None:
        request("POST", create_path, create_value)
    else:
        # The new value carries the per-host @id; Caddy re-indexes IDs on every change.
        request("PATCH", "/id/" + quote(existing, safe=""), value)


def https_port():
    """The port of Caddy's HTTPS server: its https_port, 443 unless configured."""
    port = request("GET", "/config/apps/http/https_port")
    return int(port) if port else 443


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
        if value.get("handler") == "reverse_proxy" and not value.get("@id", "").endswith("-build-status"):
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

    A host nobody registered yet is free to claim, except the default name by
    a checkout that set BILIBI_DEV_PORT: worker checkouts set a port and must
    not take bilibi.localhost from the developer's checkout. Adopt a pre-claim
    route only if its live backend runs in this checkout or its loaded
    certificate matches this checkout's certificate. Unknown legacy ownership
    fails closed; a free port never grants ownership of a host.
    """
    if route is None:
        return not os.environ.get("BILIBI_DEV_PORT") or HOST != dev_env.DEFAULT_HOST
    claims = [value["@id"] for value in walk(route) if value.get("@id", "").startswith(OWNER_PREFIXES)]
    if claims:
        return len(claims) == 1 and claims[0] in OWNER_IDS
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
    loaded = find(CERT_ID)[1]
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


def plan(requested_port, http_only=False):
    """Print scheme, host, backend port, HTTPS port and why not https, one per line.

    The last line is empty for https, "owned" when another checkout holds the
    host, and "unavailable" when Caddy does not answer.
    """
    try:
        route = find(ROUTE_ID)[1]
        servers = request("GET", "/config/apps/http/servers") or {}
        tls_port = https_port()
        owner = not http_only and owns_route(route)
        # Do not claim a host already registered by a different launcher.
        if route is None and any(HOST in matcher.get("host", [])
                                 for server in servers.values() for entry in caddy_routes(server)
                                 for matcher in entry.get("match", [])):
            owner = False
        reason = "" if owner else "owned"
    except (ApiError, RuntimeError):
        servers, route, owner, tls_port, reason = {}, None, False, 443, "unavailable"
    port = choose_port(servers, requested_port, route if owner else None)
    print("https" if owner else "http")
    print(HOST if owner else "127.0.0.1")
    print(port)
    print(tls_port)
    print("" if http_only else reason)


def waiting_route(host):
    """Caddy's error route: a branded page while the proxy is up and Bilibi is not.

    The page rides in the config itself because Caddy may not be able to read
    this checkout. static_response expands {placeholders} in its body, so every
    brace in the page is escaped. X-Bilibi-Waiting lets the page tell its own
    answer from the app's when it polls for the backend to return.
    """
    body = WAITING_PAGE.read_text()
    if os.environ.get('BILIBI_DEV_STATUS_PORT'):
        extra = WAITING_PAGE.with_name('dev-waiting.html').read_text()
        body = body.replace('</body>', extra + '\n</body>')
    body = body.replace("{", "\\{")
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


def build_status_route(port):
    """Only the local launcher owns this loopback diagnostic upstream.

    Use the page's local-client policy, including the Windows host on WSL NAT.
    """
    port = int(port)
    if not 1024 <= port <= 65535:
        raise RuntimeError('Invalid launcher status port.')
    return {
        'match': [{'path': ['/__bilibi_dev/status']}],
        'handle': [{'handler': 'subroute', 'routes': [
            {
                'match': [{'not': [{'remote_ip': {'ranges': allowed_clients()}}]}],
                'handle': [{'handler': 'static_response', 'status_code': 403}],
                'terminal': True,
            },
            {'handle': [{
                '@id': ROUTE_ID + '-build-status',
                'handler': 'reverse_proxy',
                'headers': {'request': {'set': {'Host': [f'127.0.0.1:{port}']}}},
                'upstreams': [{'dial': f'127.0.0.1:{port}'}],
            }]},
        ]}],
        'terminal': True,
    }


def install_waiting_route(server_path, server, host):
    route = waiting_route(host)
    existing, _ = find(ERROR_ROUTE_ID)
    if existing is not None:
        request("PATCH", "/id/" + quote(existing, safe=""), route)
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
    global HOST, ROUTE_ID, TLS_POLICY_ID, CERT_ID, CERT_TAG, ERROR_ROUTE_ID, LEGACY_IDS, OWNER_PREFIXES, OWNER_IDS
    planning = len(sys.argv) == 4 and sys.argv[1] in ("--plan", "--http")
    host = sys.argv[2] if planning else sys.argv[1] if len(sys.argv) == 5 else ""
    if not dev_env.HOST_PATTERN.fullmatch(host):
        raise RuntimeError("BILIBI_DEV_HOST must be a name under .localhost, such as my-bilibi.localhost.")
    HOST = host
    ROUTE_ID, TLS_POLICY_ID, CERT_ID, ERROR_ROUTE_ID = (
        f"{BASES[kind]}-{host}" for kind in ("route", "tls_policy", "certificate", "error_route")
    )
    CERT_TAG = CERT_ID
    LEGACY_IDS = {f"{base}-{host}": base for base in BASES.values()} if host == dev_env.DEFAULT_HOST else {}
    # The owner claim lives inside the route; the pre-per-host form is accepted
    # for bilibi.localhost so the developer's existing route stays theirs.
    checkout = hashlib.sha256(str(ROOT).encode()).hexdigest()
    OWNER_PREFIXES = tuple(f"{route_id}-owner-" for route_id in (ROUTE_ID, *([LEGACY_IDS[ROUTE_ID]] if ROUTE_ID in LEGACY_IDS else [])))
    OWNER_IDS = {prefix + checkout for prefix in OWNER_PREFIXES}
    OWNER_ID = ROUTE_ID + "-owner-" + checkout
    requested_port = int(sys.argv[3] if planning else sys.argv[2])
    if not 1024 <= requested_port <= 65535:
        raise RuntimeError("BILIBI_DEV_PORT must be 1024..65535.")
    if planning:
        # Even after losing a first-launch race, avoid the new owner's reserved
        # backend port while its app is still building and not listening yet.
        plan(requested_port, http_only=sys.argv[1] == "--http")
        return
    if len(sys.argv) != 5:
        raise RuntimeError("usage: local-https-api.py HOST PORT CERTIFICATE KEY")
    # local-https.sh serializes registration. Recheck even after --plan, and
    # before any TLS or HTTP write, so a port override cannot steal the host.
    own_id, own_route = find(ROUTE_ID)
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
    tls_port = https_port()
    candidates = [
        (name, server)
        for name, server in config.items()
        if any(listen.endswith(f":{tls_port}") for listen in server.get("listen", []))
    ]
    if not candidates:
        raise RuntimeError(f"Caddy has no HTTPS server listening on :{tls_port}.")
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
    existing_policy, _ = find(TLS_POLICY_ID)
    if existing_policy is not None:
        request("PATCH", "/id/" + quote(existing_policy, safe=""), tls_policy)
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
    if status_port := os.environ.get('BILIBI_DEV_STATUS_PORT'):
        route_config['handle'][0]['routes'].insert(1, build_status_route(status_port))
    if own_id is None:
        if "routes" in server:
            request("POST", server_path + "/routes", route_config)
        else:
            request("PUT", server_path + "/routes", [route_config])
    else:
        request("PATCH", "/id/" + quote(own_id, safe=""), route_config)
    install_waiting_route(server_path, server, host)
    print(port)
    print(tls_port)


if __name__ == "__main__":
    try:
        main()
    except (ApiError, OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        print(str(error).splitlines()[0], file=sys.stderr)
        raise SystemExit(1)
