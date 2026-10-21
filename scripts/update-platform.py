#!/usr/bin/env python3
"""Safe Platform updates (ADR 03); no Bilimbi module counterpart.

Only discovery's exact output and a lock reproduced from the committed lock
may be put aside. All branch decisions happen before changing the index or files.
"""
import importlib.util
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile

import catalogue

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('discovery', ROOT / 'scripts/discover-modules.py')
discovery = importlib.util.module_from_spec(spec)
spec.loader.exec_module(discovery)
Refused = discovery.Refused


def run(folder, *args, check=True, **kwargs):
    result = subprocess.run(args, cwd=folder, capture_output=True, **kwargs)
    if check and result.returncode:
        if args[:2] in (('git', 'switch'), ('git', 'merge'), ('git', 'checkout')):
            detail = result.stderr.decode().strip()
            raise Refused(f'{folder}: Git stopped the update:\n{detail}\n'
                          f'Next: {action(folder, "status", "--short", "--ignored")}. '
                          'Preserve the named blocking files or resolve the branch conflict, then retry ./bilibi update.')
        # Do not echo remote URLs or credential-helper output.
        raise Refused(f'{folder}: {shlex.join(args[:2])} failed. '
                      f'Run {shlex.join(["git", "-C", str(folder), "status"])} and resolve it before retrying.')
    return result


def git(folder, *args):
    return run(folder, 'git', *args).stdout.decode().rstrip('\n')


def action(folder, *args):
    return shlex.join(['git', '-C', str(folder), *args])


def ancestor(folder, older, newer):
    result = run(folder, 'git', 'merge-base', '--is-ancestor', older, newer, check=False)
    if result.returncode not in (0, 1):
        raise Refused(f'{folder}: cannot compare commits. Run {action(folder, "fetch", "--unshallow")} if this is a shallow clone.')
    return result.returncode == 0


def changes(folder):
    records = git(folder, 'status', '--porcelain=v1', '-z', '--untracked-files=all').split('\0')
    result = []
    index = 0
    while index < len(records):
        record = records[index]
        index += 1
        if record:
            result.append((record[:2], record[3:]))
            if 'R' in record[:2] or 'C' in record[:2]:
                index += 1
    return result


def refuse_edits(folder, paths):
    raise Refused(f'{folder}: local changes in {", ".join(paths)}. Review them with '
                  f'{action(folder, "status", "--short")}; commit your source work, then retry ./bilibi update.')


def generated_changes():
    """Prove both staged and working forms; never exempt a file by name alone."""
    generated = discovery.outputs(ROOT, discovery.discovered(ROOT))
    generated = {str(path.relative_to(ROOT)): content.encode() for path, content in generated.items()}
    dirty = changes(ROOT)
    allowed = set(generated) | {'Cargo.lock'}
    real = [path for state, path in dirty if path not in allowed or state not in (' M', 'M ', 'MM')]
    if real:
        refuse_edits(ROOT, real)
    if not dirty:
        return []
    committed = {path: run(ROOT, 'git', 'show', f'HEAD:{path}').stdout for _, path in dirty}
    if any(path == 'Cargo.lock' for _, path in dirty):
        # Resolve the committed lock against today's exact mounts in a disposable
        # copy. Cargo cannot accidentally rewrite the user's lock on refusal.
        with tempfile.TemporaryDirectory(prefix='bilibi-update-') as temporary:
            trial = Path(temporary)
            ignore = shutil.ignore_patterns('.git', '.local', 'target', 'node_modules', '__pycache__')
            for name in ('apps', 'fixtures', '.cargo'):
                if (ROOT / name).is_dir():
                    shutil.copytree(ROOT / name, trial / name, ignore=ignore, symlinks=True)
            shutil.copyfile(ROOT / 'Cargo.toml', trial / 'Cargo.toml')
            (trial / 'Cargo.lock').write_bytes(committed['Cargo.lock'])
            for path, content in generated.items():
                (trial / path).write_bytes(content)
            resolved = run(trial, 'cargo', 'metadata', '--format-version', '1', check=False)
            if resolved.returncode:
                raise Refused(f'{ROOT}/Cargo.lock: cannot verify the generated lock. '
                              'Run cargo metadata --format-version 1 in this clone, resolve its diagnostics, then retry.')
            generated['Cargo.lock'] = (trial / 'Cargo.lock').read_bytes()
    for _, path in dirty:
        file = ROOT / path
        if file.is_symlink() or not file.is_file():
            refuse_edits(ROOT, [path])
        forms = [file.read_bytes(), run(ROOT, 'git', 'show', f':{path}').stdout]
        if any(form not in (committed[path], generated[path]) for form in forms):
            refuse_edits(ROOT, [path])
        if run(ROOT, 'git', 'diff', 'HEAD', '--summary', '--', path).stdout:
            refuse_edits(ROOT, [path])
    return [path for _, path in dirty]


def integrated(folder, head, default):
    """Accept ancestry or one exact squash patch, never a subject-line guess."""
    if ancestor(folder, head, default):
        return True
    base = git(folder, 'merge-base', head, default)
    patch = run(folder, 'git', 'diff', '--binary', base, head).stdout
    if not patch:
        return False
    wanted = run(folder, 'git', 'patch-id', '--stable', input=patch).stdout.split()
    history = run(folder, 'git', 'log', '--no-merges', '-p', '--binary', f'{base}..{default}').stdout
    patches = run(folder, 'git', 'patch-id', '--stable', input=history).stdout.splitlines()
    if not wanted or not any(line.split()[0] == wanted[0] for line in patches):
        return False
    # A matching historical squash could since have been reverted. Prove the
    # patch still exists at the destination, using an isolated Git index.
    with tempfile.TemporaryDirectory(prefix='bilibi-update-index-') as temporary:
        env = {**os.environ, 'GIT_INDEX_FILE': str(Path(temporary) / 'index')}
        run(folder, 'git', 'read-tree', default, env=env)
        # Ignore surrounding context: main may have gained adjacent lines after
        # the squash. The full patch ID above already proved the exact change.
        patch = run(folder, 'git', 'diff', '--binary', '--unified=0', base, head).stdout
        return run(folder, 'git', 'apply', '--cached', '--reverse', '--check',
                   '--unidiff-zero', input=patch, env=env, check=False).returncode == 0


def branch_plan():
    branch = git(ROOT, 'branch', '--show-current')
    if not branch:
        raise Refused(f'{ROOT}: HEAD is detached. Run {action(ROOT, "switch", "<your-branch>")} and retry.')
    remote = run(ROOT, 'git', 'config', '--get', f'branch.{branch}.remote', check=False).stdout.decode().strip() or 'origin'
    if remote == '.':
        raise Refused(f'{ROOT}: {branch} tracks a local branch. Set its published upstream with '
                      f'{action(ROOT, "branch", "--set-upstream-to=origin/" + branch)} and retry.')
    previous = git(ROOT, 'for-each-ref', '--format=%(upstream)', f'refs/heads/{branch}')
    published_before = bool(previous) and run(ROOT, 'git', 'rev-parse', '--verify', previous, check=False).returncode == 0
    published_before = published_before and ancestor(ROOT, 'HEAD', previous)
    # Fetch only changes refs; it leaves all source files and the index alone.
    git(ROOT, 'fetch', '--prune', '--', remote)
    default_reply = git(ROOT, 'ls-remote', '--symref', remote, 'HEAD')
    defaults = [line.split()[1].removeprefix('refs/heads/') for line in default_reply.splitlines()
                if line.startswith('ref: refs/heads/') and line.endswith('\tHEAD')]
    if len(defaults) != 1:
        raise Refused(f'{ROOT}: cannot find the remote default branch. Run {action(ROOT, "remote", "show", remote)} and check its HEAD branch.')
    default = defaults[0]
    default_ref = f'refs/remotes/{remote}/{default}'
    git(ROOT, 'rev-parse', '--verify', default_ref)
    upstream = git(ROOT, 'for-each-ref', '--format=%(upstream)', f'refs/heads/{branch}')
    exists = bool(upstream) and run(ROOT, 'git', 'rev-parse', '--verify', upstream, check=False).returncode == 0
    covered = branch != default and integrated(ROOT, 'HEAD', default_ref)
    if covered:
        if not (ancestor(ROOT, 'HEAD', default_ref) or published_before
                or (exists and ancestor(ROOT, 'HEAD', upstream))):
            raise Refused(f'{ROOT}: cannot prove {branch} has no unpublished commits. '
                          f'Run {action(ROOT, "push", "-u", remote, branch)}, then retry.')
        local = f'refs/heads/{default}'
        if run(ROOT, 'git', 'rev-parse', '--verify', local, check=False).returncode == 0:
            if not ancestor(ROOT, local, default_ref):
                raise Refused(f'{ROOT}: local {default} has commits absent from its remote. '
                              f'Run {action(ROOT, "log", "--oneline", default_ref + ".." + local)}; publish or reconcile them before retrying.')
        reason = 'its upstream is gone and its changes are already in' if not exists else 'its changes are already in'
        print(f'{branch}: {reason} {default}. Switching safely to {default}.', flush=True)
        return default, default_ref
    if not exists:
        raise Refused(f'{ROOT}: {branch} has no available upstream and has work absent from {default}. '
                      f'Publish it with {action(ROOT, "push", "-u", remote, branch)}, then retry. No branch was switched.')
    if not ancestor(ROOT, 'HEAD', upstream):
        raise Refused(f'{ROOT}: {branch} has local commits or has diverged from its upstream. '
                      f'Run {action(ROOT, "log", "--oneline", upstream + "..HEAD")}; '
                      f'publish with {action(ROOT, "push", remote, branch)} or reconcile the branch before retrying.')
    return branch, upstream


def component_update(folder, facts, platform_version, platform_head, tested):
    """Return a plain reason when a mount is left alone; move only a clean release checkout."""
    if folder.is_symlink() or Path(git(folder, 'rev-parse', '--show-toplevel')).resolve() != folder:
        return 'not an independent real checkout; preserve it and follow its mounting guide'
    branch = git(folder, 'branch', '--show-current')
    if branch:
        return f'on branch {branch}, left as is; update it in its own repository'
    if changes(folder):
        return f'local changes; review {action(folder, "status", "--short")} and commit your work before retrying'
    if not facts:
        return 'not listed in the official catalogue; update this repository deliberately with its owner'
    repository = facts['repository'].removesuffix('.git').rstrip('/')
    origin = git(folder, 'remote', 'get-url', 'origin').removesuffix('.git').rstrip('/')
    if not repository.startswith('https://github.com/BelimbingApp/') or origin != repository:
        return f'origin differs from the official catalogue; inspect {action(folder, "remote", "-v")} and reconcile it deliberately'
    head = git(folder, 'rev-parse', 'HEAD')
    if head not in {release['commit'] for release in facts['versions'].values()}:
        return f'detached at {head[:12]}, which is not an official release commit; left as is'
    choices = []
    for version, release in facts['versions'].items():
        if catalogue.satisfies(platform_version, catalogue.parse_requirement(release['platform'], str(folder))):
            checked = any(item['platform']['commit'] == platform_head
                          and item['platform']['version'] == platform_version
                          and item['domain']['id'] == folder.name
                          and item['domain']['commit'] == release['commit']
                          and item['domain']['version'] == version for item in tested)
            choices.append((checked, catalogue.version_tuple(version), version, release['commit']))
    if not choices:
        return f'no release supports Platform {platform_version}; ask the component owner to publish a compatible release'
    _, _, version, target = max(choices)
    if head == target:
        print(f'{folder}: already at recommended {version} ({head[:12]}).', flush=True)
        return None
    installed = max((v for v, release in facts['versions'].items() if release['commit'] == head), key=catalogue.version_tuple)
    if catalogue.version_tuple(version) < catalogue.version_tuple(installed):
        return f'recommended {version} is older than installed {installed}; update never downgrades'
    git(folder, 'fetch', '--prune', '--tags', 'origin')
    if run(folder, 'git', 'cat-file', '-e', f'{target}^{{commit}}', check=False).returncode:
        return f'recommended commit {target} is unavailable; ask the component owner to publish it'
    git(folder, 'checkout', '--no-overwrite-ignore', '--detach', target)
    print(f'{folder}: {head[:12]} -> recommended {version} ({target[:12]}).', flush=True)
    return None


def update_components(remote):
    """Best effort once the Platform has moved: a catalogue problem skips this step only."""
    with tempfile.TemporaryDirectory(prefix='bilibi-update-catalogue-') as temporary:
        # main owns membership; the published catalogue branch supplies newer releases and tested sets.
        published = Path(temporary) / 'catalogue.json'
        shown = run(ROOT, 'git', 'show', f'refs/remotes/{remote}/catalogue:catalogue.json', check=False)
        if shown.returncode == 0:
            published.write_bytes(shown.stdout)
        try:
            data = catalogue.combined(ROOT / 'catalogue.json', published, [])
        except (catalogue.Refused, KeyError, TypeError) as error:
            print(f'Component updates skipped: the catalogue cannot be read ({error}). '
                  'Components were left as they are; retry ./bilibi update later.', flush=True)
            return
    version = discovery.workspace_version(ROOT)
    head = git(ROOT, 'rev-parse', 'HEAD')
    for layer in ('domains', 'extensions'):
        for folder in sorted((ROOT / 'apps' / layer).iterdir()):
            if not (folder.is_dir() or folder.is_symlink()) or folder.name == 'src' or folder.name.startswith('.'):
                continue
            try:
                reason = component_update(folder, data.get(layer, {}).get(folder.name), version, head,
                                          data['tested'] if layer == 'domains' else [])
            except (Refused, catalogue.Refused) as error:
                reason = str(error)
            if reason:
                print(f'Left alone: {folder}: {reason}.', flush=True)


def update():
    generated_changes()
    branch, upstream = branch_plan()
    before = git(ROOT, 'rev-parse', 'HEAD')
    remote = upstream.removeprefix('refs/remotes/').split('/', 1)[0]
    # Fetches can take time. Recheck before setting anything aside, so edits
    # made while the network was busy cannot become disposable composition.
    paths = generated_changes()
    # Back up exact working and index bytes, including a staged generated form.
    saved = {path: ((ROOT / path).read_bytes(), run(ROOT, 'git', 'show', f':{path}').stdout) for path in paths}
    try:
        if paths:
            print('Regenerating local composition: ' + ', '.join(paths), flush=True)
            git(ROOT, 'restore', '--source=HEAD', '--staged', '--worktree', '--', *paths)
        if git(ROOT, 'branch', '--show-current') != branch:
            if run(ROOT, 'git', 'show-ref', '--verify', f'refs/heads/{branch}', check=False).returncode:
                git(ROOT, 'switch', '--no-overwrite-ignore', '--track', '-c', branch, upstream)
            else:
                git(ROOT, 'switch', '--no-overwrite-ignore', branch)
        git(ROOT, 'merge', '--ff-only', '--no-overwrite-ignore', upstream)
    except Refused:
        # A failed switch/fast-forward must not consume generated work either.
        for path, (working, staged) in saved.items():
            blob = run(ROOT, 'git', 'hash-object', '-w', '--stdin', input=staged).stdout.decode().strip()
            mode = git(ROOT, 'ls-files', '--stage', '--', path).split()[0]
            git(ROOT, 'update-index', '--cacheinfo', mode, blob, path)
            (ROOT / path).write_bytes(working)
        raise
    after = git(ROOT, 'rev-parse', 'HEAD')
    print(f'Platform: {before[:12]} -> {after[:12]} on {branch}.' if before != after
          else f'Platform: already up to date on {branch} ({after[:12]}).', flush=True)
    if before != after:
        print(git(ROOT, 'diff', '--stat', before, after), flush=True)
    update_components(remote)
    subprocess.run([sys.executable, str(ROOT / 'scripts/discover-modules.py')], cwd=ROOT, check=True)
    resolved = subprocess.run(['cargo', 'metadata', '--format-version', '1'], cwd=ROOT, stdout=subprocess.DEVNULL)
    if resolved.returncode:
        raise Refused(f'{ROOT}: Platform updated, but Cargo could not resolve the mounted composition. '
                      'Resolve the diagnostics above, run cargo metadata --format-version 1, then ./bilibi start.')
    print('Composition regenerated for the mounted components.\nNext: ./bilibi start', flush=True)


if __name__ == '__main__':
    try:
        update()
    except (Refused, catalogue.Refused, OSError, ValueError, subprocess.CalledProcessError) as error:
        raise SystemExit(f'bilibi update: {error}') from None
