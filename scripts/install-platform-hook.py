#!/usr/bin/env python3
"""Install the Platform commit guard, chaining a repository-local hook.

Policy lives in discover-modules.py --check --staged;
this file only installs it. Never change core.hooksPath or a hook outside this repository.
See README.md for conflicts and recovery.
"""
import argparse
import fcntl
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent
EARLIER = b'# Bilibi Platform pre-commit safeguard v1; managed by scripts/install-platform-hook.py.'
HOOK = '''#!/usr/bin/env bash
# Bilibi Platform pre-commit safeguard v2; managed by scripts/install-platform-hook.py.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
if ! python3 "$root/scripts/discover-modules.py" --check --staged; then
    echo "Platform commit refused in $root. See README.md (Commit safeguard) for recovery." >&2
    exit 1
fi
hooks=$(git rev-parse --path-format=absolute --git-path hooks)
if [[ -x $hooks/pre-commit.bilibi-original ]]; then
    exec "$hooks/pre-commit.bilibi-original" "$@"
fi
'''


def git(*args):
    return subprocess.check_output(['git', '-C', str(ROOT), *args], text=True).strip()


class Refused(Exception):
    pass


def refuse(reason):
    raise Refused(f'Platform safeguard: {reason}. Left existing hooks and core.hooksPath unchanged. '
                  'See README.md (Commit safeguard) for the repository-local integration steps.')


def install():
    if Path(git('rev-parse', '--show-toplevel')).resolve() != ROOT:
        refuse(f'{ROOT} is not the Platform Git root')
    common = Path(git('rev-parse', '--path-format=absolute', '--git-common-dir')).resolve()
    hooks = Path(git('rev-parse', '--path-format=absolute', '--git-path', 'hooks'))
    resolved = hooks.resolve()
    if not any(resolved.is_relative_to(folder) for folder in (ROOT, common)):
        refuse(f'the hooks directory {hooks} is outside this repository')
    # A symlink can redirect the installer or make its wrapper affect other repos.
    if any(folder.is_symlink() for folder in (hooks, *hooks.parents)):
        refuse(f'the hooks directory {hooks} uses a symlink')
    hooks.mkdir(parents=True, exist_ok=True)
    with (common / 'bilibi-hook-install.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        hook = hooks / 'pre-commit'
        original = hooks / 'pre-commit.bilibi-original'
        if hook.is_symlink() or original.is_symlink():
            refuse(f'{hook} or its backup is a symlink')
        if hook.is_file() and hook.read_bytes() == HOOK.encode():
            if not os.access(hook, os.X_OK):
                refuse(f'{hook} is not executable; run chmod +x on it')
            print(f'Platform safeguard verified: {hook}')
            return
        earlier = hook.is_file() and hook.read_bytes().split(b'\n')[1:2] == [EARLIER]
        if original.exists() and not earlier:
            refuse(f'{original} already exists but {hook} is not the managed guard')
        if hook.exists() and not hook.is_file():
            refuse(f'{hook} is not a regular file')
        if hook.is_relative_to(ROOT) and git('ls-files', '--', str(hook.relative_to(ROOT))):
            refuse(f'{hook} is tracked; integrate the discovery check in that hook explicitly')
        fd, temporary = tempfile.mkstemp(prefix='.bilibi-pre-commit-', dir=hooks)
        try:
            with os.fdopen(fd, 'w') as output:
                output.write(HOOK)
                os.fchmod(output.fileno(), 0o755)
            if hook.exists() and not earlier:
                hook.rename(original)
            try:
                os.replace(temporary, hook)
            except OSError:
                if not earlier and original.exists():
                    original.rename(hook)
                raise
        finally:
            Path(temporary).unlink(missing_ok=True)
        print(f'Platform safeguard installed: {hook}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--warn', action='store_true',
                        help='report a conflict and exit 0, for commands that do not mount components')
    arguments = parser.parse_args()
    try:
        install()
    except (Refused, OSError, subprocess.CalledProcessError) as error:
        message = str(error) if isinstance(error, Refused) else (
            f'Platform safeguard installation failed: {error}. See README.md (Commit safeguard).')
        if not arguments.warn:
            raise SystemExit(message) from None
        print(f'{message}\nContinuing without the safeguard: ./bilibi add refuses until this is resolved, '
              'and CI still rejects mounted composition files.', file=sys.stderr)
