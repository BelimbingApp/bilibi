#!/usr/bin/env python3
"""Keep a trial's source composition; no Bilimbi module counterpart (ADR 03)."""
import argparse
import importlib.util
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parent.parent
# Version the application tooling independently of the selected Platform sources.
TEMPLATE = 'https://github.com/BelimbingApp/bilibi-app-maker.git'
TEMPLATE_COMMIT = '481ad710b70bec905bf9c1f5b8eaf2bb09b24d35'


class Refused(Exception):
    pass


def run(*args, cwd=ROOT, capture=False, env=None):
    return subprocess.run(list(map(str, args)), cwd=cwd, check=True, text=True,
                          stdout=subprocess.PIPE if capture else None, env=env).stdout


def git(folder, *args):
    return run('git', '-C', folder, *args, capture=True).rstrip('\n')


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def source_url(folder):
    remote = subprocess.run(['git', '-C', str(folder), 'remote', 'get-url', 'origin'],
                            capture_output=True, text=True)
    if remote.returncode:
        command = shlex.join(['git', '-C', str(folder), 'remote', 'add', 'origin'])
        raise Refused(f'{folder}: no origin is configured. Add its published source:\n'
                      f'  {command} <credential-free-https-url>\nThen publish the current commit and retry ./bilibi new.')
    url = remote.stdout.rstrip('\n')
    parsed = urlsplit(url)
    if (parsed.scheme != 'https' or not parsed.hostname or parsed.username or parsed.password
            or parsed.query or parsed.fragment or parsed.port):
        raise Refused(f'{folder}: origin must be an HTTPS Git URL without credentials, query or fragment. '
                      'Set a credential-free origin with git remote set-url origin <https-url>; '
                      'use a Git credential helper for private sources.')
    return url if url.endswith('.git') else url + '.git'


def selections():
    discovery = load('discovery', ROOT / 'scripts/discover-modules.py')
    try:
        modules = discovery.discovered(ROOT)
    except discovery.Refused as error:
        raise Refused(str(error)) from None
    generated = discovery.outputs(ROOT, modules)
    allowed = {str(path.relative_to(ROOT)) for path in generated} | {'Cargo.lock'}
    for path, content in generated.items():
        if not path.is_file() or path.read_text() != content:
            raise Refused(f'{path}: generated composition differs. Preserve any intentional edits, then run '
                          'python3 scripts/discover-modules.py before ./bilibi new.')
    mounted = discovery.mounted_containers(ROOT, modules)
    for layer in ('domains', 'extensions'):
        for folder in (ROOT / 'apps' / layer).iterdir():
            if folder.is_dir() and folder.name != 'src' and not folder.name.startswith('.'):
                if f'{layer}/{folder.name}' not in mounted:
                    raise Refused(f'{folder}: has no discoverable modules; preserve it outside the mount before retrying. '
                                  'An empty component cannot be represented by the composition manifest.')
    folders = [('platform', ROOT)] + [(f'apps/{name}', folder) for name, folder in sorted(mounted.items())]
    selected = {'format': 1, 'targets': ['x86_64-unknown-linux-gnu'], 'domains': [], 'extensions': []}
    for name, folder in folders:
        if folder.is_symlink() or Path(git(folder, 'rev-parse', '--show-toplevel')).resolve() != folder:
            raise Refused(f'{folder}: expected an independent real Git checkout')
        changes = git(folder, 'status', '--porcelain=v1', '-z', '--untracked-files=all').split('\0')
        unexpected = []
        index = 0
        while index < len(changes):
            change = changes[index]
            index += 1
            if not change:
                continue
            status, path = change[:2], change[3:]
            if 'R' in status or 'C' in status:
                index += 1
            if name != 'platform' or path not in allowed or status not in (' M', 'M ', 'MM'):
                unexpected.append(path)
        if unexpected:
            command = shlex.join(['git', '-C', str(folder), 'add', '--', *unexpected])
            commit = shlex.join(['git', '-C', str(folder), 'commit', '-m', 'Save application source changes'])
            raise Refused(f'{folder}: uncommitted source changes. Review and commit them in their owning repository:\n'
                          f'  {command}\n  {commit}\nThen push that commit and retry ./bilibi new.')
        source = {'url': source_url(folder), 'commit': git(folder, 'rev-parse', 'HEAD')}
        if name == 'platform':
            selected['platform'] = source
        else:
            _, layer, identity = name.split('/')
            selected[layer].append({'id': identity, **source})
    return selected, folders


def independent_clone(source, destination, env, original=None):
    try:
        run('git', 'clone', '--no-local', '--no-checkout', '--', source['url'], destination, env=env)
        run('git', 'fetch', '--tags', 'origin', source['commit'], cwd=destination, env=env)
        run('git', 'checkout', '--detach', source['commit'], cwd=destination, env=env)
    except subprocess.CalledProcessError:
        if original is None:
            raise Refused('Could not fetch the app maker. Check network and Git credentials, then retry.') from None
        push = shlex.join(['git', '-C', str(original), 'push', 'origin',
                          f'{source["commit"]}:refs/heads/bilibi-save-{source["commit"][:12]}'])
        raise Refused(f'{original}: cannot fetch {source["commit"]} from its declared origin. '
                      f'Check network and credentials; if unpublished, publish this exact commit:\n  {push}\n'
                      'Then retry ./bilibi new; no substitute revision was selected.') from None


def create(name):
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]*', name):
        raise Refused('Use a sibling folder name containing letters, digits, hyphens or underscores: ./bilibi new my-bilibi')
    destination = ROOT.parent / name
    if destination.exists() or destination.is_symlink():
        raise Refused(f'{destination} is occupied. Choose another name: ./bilibi new <unused-name>. '
                      'Existing files were left untouched.')
    selected, folders = selections()
    # Neither inherited Git object paths nor application output overrides may
    # redirect independent reconstruction into the trial or another workspace.
    env = dict(os.environ)
    for key in ('GIT_DIR', 'GIT_COMMON_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE',
                'GIT_OBJECT_DIRECTORY', 'GIT_ALTERNATE_OBJECT_DIRECTORIES'):
        env.pop(key, None)
    for key in ('BILIBI_SOURCE_DIR', 'BILIBI_BUILD_DIR', 'CARGO_TARGET_DIR'):
        env.pop(key, None)
    with tempfile.TemporaryDirectory(prefix='.bilibi-new-', dir=ROOT.parent) as temp:
        application = Path(temp) / 'application'
        independent_clone({'url': TEMPLATE, 'commit': TEMPLATE_COMMIT}, application, env)
        # Keep the maker's history so maker updates merge later:
        # it becomes the `upstream` remote, and `main` starts at the pinned
        # commit, tracking nothing until the application adds its `origin`.
        # Its tags are not the application's releases.
        run('git', 'remote', 'rename', 'origin', 'upstream', cwd=application, env=env)
        branches = git(application, 'for-each-ref', '--format=%(refname:short)', 'refs/heads').split()
        if branches:
            run('git', 'branch', '--quiet', '--delete', '--force', *branches, cwd=application, env=env)
        run('git', 'checkout', '--quiet', '-b', 'main', TEMPLATE_COMMIT, cwd=application, env=env)
        tags = git(application, 'tag', '--list').split()
        if tags:
            run('git', 'tag', '--delete', *tags, cwd=application, capture=True, env=env)
        # Use the maker's own recipe writer and validators.
        sys.path.insert(0, str(application / 'scripts'))
        workspace = load('application_workspace', application / 'scripts/workspace.py')
        (application / 'setup.toml').write_text(workspace.serialize(selected))
        workspace.recipe.recipe(application)
        for name, original in folders:
            source = selected['platform'] if name == 'platform' else next(
                item for item in selected[name.split('/')[1]] if item['id'] == name.split('/')[2])
            folder = application / 'sources/platform'
            if name != 'platform':
                folder /= name
            folder.parent.mkdir(parents=True, exist_ok=True)
            independent_clone(source, folder, env, original)
        platform = application / 'sources/platform'
        # Capture the trial's dependency selection, never resolve a replacement.
        shutil.copyfile(ROOT / 'Cargo.lock', platform / 'Cargo.lock')
        run('python3', 'scripts/discover-modules.py', cwd=platform, env=env)
        try:
            run('python3', 'scripts/discover-modules.py', '--pin', cwd=platform, env=env)
        except subprocess.CalledProcessError:
            raise Refused('The trial Cargo.lock does not validate against its independently fetched sources. '
                          'Run cargo metadata in the trial to resolve its mounted composition, then retry.') from None
        for file in ('Cargo.lock', 'manifest.txt'):
            shutil.copyfile(platform / '.local/composition' / file, application / 'composition' / file)
        run('python3', 'scripts/recipe.py', 'restore', cwd=application, env=env)
        if selections()[0] != selected or (ROOT / 'Cargo.lock').read_bytes() != (application / 'composition/Cargo.lock').read_bytes():
            raise Refused('Trial sources changed during reconstruction. Retry ./bilibi new with stable sources.')
        # Atomic reservation refuses even an empty directory that appeared while
        # reconstructing; never rename over another person's destination.
        destination.mkdir()
        try:
            for child in application.iterdir():
                child.rename(destination / child.name)
        except BaseException:
            raise Refused(f'Creation interrupted; the new files remain at {destination} for inspection. Trial unchanged.') from None
    print(f'Created {destination}. Independent fetches reconstructed every recorded commit.\n'
          'Only sources and their dependency lock were captured. Database, media, configuration and keys stay in the trial.\n'
          f'Next:\n  cd {shlex.quote(str(destination))}\n  ./bilibi start\n'
          'Review setup.toml and composition/, then commit your application files.\n'
          'When ready, create your own remote, run git remote add origin <your-repository-url>, and git push -u origin main.\n'
          'The app maker is the upstream remote; its README explains how to take its updates.')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('name')
    args = parser.parse_args()
    try:
        create(args.name)
    except (Refused, OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(f'bilibi new: {error}\nTrial checkout and runtime data were left untouched.')
