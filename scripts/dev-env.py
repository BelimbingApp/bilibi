#!/usr/bin/env python3
"""Resolve the local launcher's settings: the shell first, then .env, then defaults.

Local launcher plumbing. start-app.sh
evaluates the `export` lines this prints; nothing else reads .env, and a
server never does (install-server.sh writes each instance's own env file).

Each setting comes from the first place that has it: a variable set in the
shell, the .env file beside the ./bilibi front door you ran (the application
root for an app-maker application, otherwise this checkout), then its default.
Every checkout defaults to bilibi.localhost. Set BILIBI_DEV_HOST in .env to
choose another address. .env.example at the Platform root lists every line
.env accepts.
"""
import os
from pathlib import Path
import re
import shlex
import sys


ROOT = Path(__file__).resolve().parent.parent
HOST_PATTERN = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)*(?:\.[a-z0-9]+(?:-[a-z0-9]+)*)*\.localhost")
DEFAULT_HOST = "bilibi.localhost"
# The lines .env may set, each with its check. Keep .env.example in step.
SETTINGS = {
    "BILIBI_DEV_HOST": lambda value: bool(HOST_PATTERN.fullmatch(value)),
    "BILIBI_DEV_PORT": lambda value: value.isdigit() and 1024 <= int(value) <= 65535,
    "BILIBI_OPEN_BROWSER": lambda value: value in ("0", "1"),
}


def application_root():
    """The app-maker application this checkout serves, or None for a Platform clone.

    BILIBI_APPLICATION_ROOT names it outright; the maker sets it when its
    sources live elsewhere (BILIBI_SOURCE_DIR). Otherwise the documented layout
    <application>/sources/platform identifies it (docs/application-workspaces.md).
    """
    named = os.environ.get("BILIBI_APPLICATION_ROOT")
    if named:
        root = Path(named).resolve()
        if not root.is_dir():
            raise ValueError(f"BILIBI_APPLICATION_ROOT is not a directory: {named}")
        return root
    candidate = ROOT.parent.parent
    if ROOT.name == "platform" and ROOT.parent.name == "sources" and (candidate / "setup.toml").is_file():
        return candidate
    return None


def read_env(path):
    """NAME=value lines, one per line; # starts a comment line; quotes are optional."""
    values = {}
    if not path.is_file():
        return values
    for number, raw in enumerate(path.read_text().splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        line = line.removeprefix("export ").lstrip()
        key, separator, value = line.partition("=")
        key, value = key.strip(), value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        if not separator or not re.fullmatch(r"[A-Z][A-Z0-9_]*", key):
            print(f"{path}:{number}: expected NAME=value; this line was ignored.", file=sys.stderr)
        elif key not in SETTINGS:
            print(f"{path}:{number}: {key} is not a local launcher setting (see .env.example); it was ignored.",
                  file=sys.stderr)
        elif not SETTINGS[key](value):
            raise ValueError(f"{path}:{number}: {key}={value} is not a valid value; see .env.example.")
        else:
            values[key] = value
    return values


def main():
    application = application_root()
    env_file = (application or ROOT) / ".env"
    # An empty shell variable counts as unset, as ${VAR:-} does in the launcher.
    exports = {key: value for key, value in read_env(env_file).items() if not os.environ.get(key)}
    if exports:
        print(f"{env_file}: {', '.join(sorted(exports))}", file=sys.stderr)
    host = os.environ.get("BILIBI_DEV_HOST") or exports.get("BILIBI_DEV_HOST")
    if not host:
        exports["BILIBI_DEV_HOST"] = DEFAULT_HOST
    elif not HOST_PATTERN.fullmatch(host):
        raise ValueError("BILIBI_DEV_HOST must be a name under .localhost, such as my-bilibi.localhost.")
    # The launcher names this file when it tells someone how to rename an instance.
    exports["BILIBI_DEV_ENV_FILE"] = str(env_file)
    for key, value in exports.items():
        print(f"export {key}={shlex.quote(value)}")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError) as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(1)
