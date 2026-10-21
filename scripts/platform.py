#!/usr/bin/env python3
"""Platform clone add/status plumbing; no Bilimbi module counterpart.

Mount conventions and validation belong to discover-modules.py. Application
recipes and `new` are later work under ADR 03.
"""
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('discovery', ROOT / 'scripts/discover-modules.py')
discovery = importlib.util.module_from_spec(spec)
spec.loader.exec_module(discovery)


def git(folder, *args):
    return discovery.git(folder, *args).rstrip('\n')


def repositories():
    yield ROOT
    for layer in ('domains', 'extensions'):
        for folder in sorted((ROOT / 'apps' / layer).iterdir()):
            if folder.is_dir() and folder.name != 'src' and not folder.name.startswith('.'):
                yield folder


def status():
    for folder in repositories():
        print(f'\n{folder}', flush=True)
        if folder.is_symlink() or Path(git(folder, 'rev-parse', '--show-toplevel')).resolve() != folder.resolve():
            raise discovery.Refused(f'{folder} is not an independent real Git checkout; see its mounting guide')
        branch = git(folder, 'branch', '--show-current') or '(detached HEAD)'
        print(f'  {branch} at {git(folder, "rev-parse", "--short=12", "HEAD")}', flush=True)
        changes = git(folder, 'status', '--short')
        print(changes or '  No local changes.', flush=True)
    print('\nNo saved application recipe exists in this Platform clone.\n'
          'Next: ./bilibi new <name> will keep your composition in its own application repository '
          '(planned; not available yet).', flush=True)


def add(component):
    if not re.fullmatch(r'[a-z][a-z0-9]*(?:_[a-z0-9]+)*', component) or component in discovery.RESERVED_IDS:
        raise discovery.Refused('use a component ID from catalogue.json, for example: ./bilibi add finance')
    catalogue = json.loads((ROOT / 'catalogue.json').read_text())
    choices = [(layer, catalogue.get(layer, {})[component]) for layer in ('domains', 'extensions')
               if component in catalogue.get(layer, {})]
    if len(choices) != 1:
        raise discovery.Refused(f'{component!r} must name exactly one official Domain or Extension in catalogue.json; '
                               'private components use the existing apps/domains or apps/extensions mounting guide')
    layer, entry = choices[0]
    repository = entry['repository']
    if not re.fullmatch(r'https://github\.com/BelimbingApp/[A-Za-z0-9][A-Za-z0-9_.-]*(?:\.git)?', repository):
        raise discovery.Refused(f'{component} is not an official BelimbingApp repository; use the manual mounting guide')
    # Install and verify even when add runs before the first start.
    subprocess.run([sys.executable, str(ROOT / 'scripts/install-platform-hook.py')], check=True)
    destination = ROOT / 'apps' / layer / component
    if destination.exists() or destination.is_symlink():
        if destination.is_symlink() or not destination.is_dir():
            raise discovery.Refused(f'{destination} is occupied; move it aside deliberately before adding {component}')
        if Path(git(destination, 'rev-parse', '--show-toplevel')).resolve() != destination.resolve():
            raise discovery.Refused(f'{destination} is not its own Git checkout; move it aside deliberately')
        expected = repository.removesuffix('.git')
        actual = git(destination, 'remote', 'get-url', 'origin').removesuffix('.git')
        if actual != expected:
            raise discovery.Refused(f'{destination} has a different origin; inspect it with git -C {destination} remote -v '
                                   'and reconcile it deliberately; no files or remotes were changed')
        print(f'Keeping existing checkout, branch and edits: {destination}', flush=True)
    else:
        subprocess.run(['git', 'clone', '--', repository, str(destination)], check=True)
    descriptor = tomllib.loads((destination / discovery.CONTAINER_FILE).read_text())
    if descriptor.get('id') != component or descriptor.get('layer') != discovery.CONTAINER_LAYERS[layer]:
        raise discovery.Refused(f'{destination}/bilibi.container.toml does not match the requested component; '
                               'checkout retained for inspection')
    modules = discovery.discovered(ROOT)
    discovery.write_outputs(discovery.outputs(ROOT, modules))
    subprocess.run(['cargo', 'metadata', '--format-version', '1', '--no-deps'], cwd=ROOT,
                   stdout=subprocess.DEVNULL, check=True)
    print(f'Added {component} at {destination}. Generated composition and Cargo.lock stay local to Platform.\n'
          'Next: ./bilibi start', flush=True)


if __name__ == '__main__':
    try:
        if sys.argv[1:] == ['status']:
            status()
        elif len(sys.argv) == 3 and sys.argv[1] == 'add':
            add(sys.argv[2])
        else:
            raise discovery.Refused('Usage: ./bilibi status | ./bilibi add <component>')
    except (discovery.Refused, OSError, ValueError, subprocess.CalledProcessError) as error:
        raise SystemExit(f'bilibi: {error}\nExisting checkouts were not reset, cleaned or switched.') from None
