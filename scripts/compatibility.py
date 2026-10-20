#!/usr/bin/env python3
"""Select official Domains and Extensions for CI (docs/versions.md).

Platform-only automation; there is no Bilimbi module counterpart.
Companion refs are PR-body data, never shell commands or repository overrides.
"""
import json
import os
import re
import subprocess
from pathlib import Path

from catalogue import CATALOGUE, Refused, check, expect, load


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


def main():
    try:
        event = json.loads(Path(os.environ['GITHUB_EVENT_PATH']).read_text())
        selected = matrix(event, os.environ['GITHUB_EVENT_NAME'])
        encoded = json.dumps(selected, separators=(',', ':'))
        print(encoded)
        with Path(os.environ['GITHUB_OUTPUT']).open('a') as output:
            output.write(f'matrix={encoded}\n')
            output.write(f'has_containers={str(bool(selected["include"])).lower()}\n')
    except (Refused, OSError, ValueError, KeyError) as refusal:
        raise SystemExit(f'compatibility: {refusal}') from None


if __name__ == '__main__':
    main()
