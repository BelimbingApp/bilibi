#!/usr/bin/env python3
"""Select official Domains and Extensions for CI (docs/versions.md).

Platform-only automation; there is no Bilimbi module counterpart.
Companion refs are PR-body data, never shell commands or repository overrides.

  compatibility.py                 write the matrix for this event
  compatibility.py affected domains|platform
                                   write affected=true|false for a pull request
"""
import json
import os
import re
import subprocess
import sys
from pathlib import Path

from catalogue import CATALOGUE, Refused, check, expect, load

ROOT = Path(__file__).resolve().parent.parent
# Files that cannot change what either workflow compiles or boots.
_HARMLESS_PREFIXES = ('docs/', '.agents/', '.amp/', '.claude/')
_HARMLESS_FILES = {'.gitignore', '.editorconfig', '.gitattributes', 'compose.yml'}


def matrix(event, event_name):
    catalogue = load(CATALOGUE)
    check(catalogue)
    containers = {}
    for role in ('domains', 'extensions'):
        for container, facts in catalogue.get(role, {}).items():
            repository = re.fullmatch(r'https://github\.com/([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)', facts['repository'])
            expect(repository, f'{role}/{container}: CI needs a GitHub repository URL')
            containers[f'{role}/{container}'] = {
                'role': role,
                'id': container,
                'repository': repository.group(1),
                'ref': 'refs/heads/main',
            }

    # Other event types ignore companion directives, including manual runs
    # whose payload might be copied from a pull request.
    overrides = set()
    if event_name == 'pull_request':
        for line in (event['pull_request'].get('body') or '').splitlines():
            if not line.strip().startswith('Bilibi-Companion:'):
                continue
            match = re.fullmatch(r'Bilibi-Companion: (domains|extensions)/([a-z][a-z0-9]*(?:_[a-z0-9]+)*)=([A-Za-z0-9_./-]+)', line.strip())
            expect(match, 'write Bilibi-Companion: domains/finance=fix/platform-change (docs/versions.md)')
            role, container, branch = match.groups()
            key = f'{role}/{container}'
            expect(key in containers, f'{key}: companion is not listed in catalogue.json')
            expect(key not in overrides, f'{key}: more than one companion branch')
            ref = f'refs/heads/{branch}'
            result = subprocess.run(['git', 'check-ref-format', ref], capture_output=True, check=False)
            expect(result.returncode == 0, f'{key}: invalid companion branch {branch!r}')
            containers[key]['ref'] = ref
            overrides.add(key)
    return {'include': list(containers.values())}


def harmless(path):
    """True when `path` cannot change a mounted build or the unmounted Rust gates."""
    name = path.rsplit('/', 1)[-1]
    return (
        path.endswith('.md')
        or path.startswith(_HARMLESS_PREFIXES)
        or path.startswith('LICENSE')
        or name.startswith('LICENSE')
        or path in _HARMLESS_FILES
    )


def affects_domains(path):
    """True when a pull request file can change a mounted Domain or Extension."""
    if harmless(path):
        return False
    if path.startswith('.github/'):
        return path == '.github/workflows/domains.yml'
    return True


def affects_platform(path):
    """True when a pull request file can change the unmounted Rust gates."""
    if harmless(path):
        return False
    if path.startswith('.github/'):
        return path == '.github/workflows/ci.yml'
    return True


def companion_requested(event):
    body = (event.get('pull_request') or {}).get('body') or ''
    return any(line.strip().startswith('Bilibi-Companion:') for line in body.splitlines())


def decide(which, event_name, event, paths):
    """Return whether CI must run, and a short reason.

    `paths` is the pull-request diff. Anything that is not a pull request runs.
    An empty diff still runs: there is nothing to prove the change is harmless.
    """
    if event_name != 'pull_request':
        return True, 'not a pull request'
    if which == 'domains' and companion_requested(event):
        return True, 'a companion branch is selected'
    if not paths:
        return True, 'the pull request diff is empty'
    predicate = affects_domains if which == 'domains' else affects_platform
    hitting = [path for path in paths if predicate(path)]
    if hitting:
        shown = ', '.join(hitting[:12])
        extra = '' if len(hitting) <= 12 else f' (+{len(hitting) - 12} more)'
        return True, shown + extra
    return False, 'only docs and other files that cannot affect this check'


def changed_paths(event):
    base = event['pull_request']['base']['sha']
    result = subprocess.run(
        ['git', 'diff', '--name-only', base, 'HEAD'],
        cwd=ROOT, check=False, capture_output=True, text=True)
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        raise SystemExit(f'compatibility: cannot diff {base}...HEAD\n{detail}')
    return [line for line in result.stdout.splitlines() if line]


def write_output(name, value):
    with Path(os.environ['GITHUB_OUTPUT']).open('a') as output:
        output.write(f'{name}={value}\n')


def affected_main(which):
    event = json.loads(Path(os.environ['GITHUB_EVENT_PATH']).read_text())
    event_name = os.environ['GITHUB_EVENT_NAME']
    paths = changed_paths(event) if event_name == 'pull_request' else None
    affected, reason = decide(which, event_name, event, paths)
    print(f'affected={str(affected).lower()}: {reason}')
    write_output('affected', str(affected).lower())
    # A skipped select step must still leave a valid matrix. fromJSON('') fails
    # the workflow even when the domain job's `if` is false.
    write_output('matrix', '{"include":[]}')
    write_output('has_containers', 'false')


def matrix_main():
    try:
        event = json.loads(Path(os.environ['GITHUB_EVENT_PATH']).read_text())
        selected = matrix(event, os.environ['GITHUB_EVENT_NAME'])
        encoded = json.dumps(selected, separators=(',', ':'))
        print(encoded)
        write_output('matrix', encoded)
        write_output('has_containers', str(bool(selected['include'])).lower())
    except (Refused, OSError, ValueError, KeyError) as refusal:
        raise SystemExit(f'compatibility: {refusal}') from None


def main(argv):
    if len(argv) == 1:
        matrix_main()
        return
    if len(argv) == 3 and argv[1] == 'affected' and argv[2] in ('domains', 'platform'):
        try:
            affected_main(argv[2])
        except (OSError, ValueError, KeyError) as refusal:
            raise SystemExit(f'compatibility: {refusal}') from None
        return
    raise SystemExit('usage: compatibility.py [affected domains|platform]')


if __name__ == '__main__':
    main(sys.argv)
