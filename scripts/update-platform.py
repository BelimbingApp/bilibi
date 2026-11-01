#!/usr/bin/env python3
"""Safe Platform updates (ADR 03).

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
from urllib.parse import urlsplit

import catalogue

remote_spec = importlib.util.spec_from_file_location('remote_org', Path(__file__).resolve().parent / 'remote-org.py')
remote_org = importlib.util.module_from_spec(remote_spec)
remote_spec.loader.exec_module(remote_org)

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('discovery', ROOT / 'scripts/discover-modules.py')
discovery = importlib.util.module_from_spec(spec)
spec.loader.exec_module(discovery)
Refused = discovery.Refused
# Repositories the user trusted for updates, one normalized address per line (ignored local state).
TRUST_FILE = ROOT / '.local/update-trust.txt'


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


def branch_plan(folder):
    branch = git(folder, 'branch', '--show-current')
    if not branch:
        raise Refused(f'{folder}: HEAD is detached. Run {action(folder, "switch", "<your-branch>")} and retry.')
    remote = run(folder, 'git', 'config', '--get', f'branch.{branch}.remote', check=False).stdout.decode().strip() or 'origin'
    if remote == '.':
        raise Refused(f'{folder}: {branch} tracks a local branch. Set its published upstream with '
                      f'{action(folder, "branch", "--set-upstream-to=origin/" + branch)} and retry.')
    previous = git(folder, 'for-each-ref', '--format=%(upstream)', f'refs/heads/{branch}')
    published_before = bool(previous) and run(folder, 'git', 'rev-parse', '--verify', previous, check=False).returncode == 0
    published_before = published_before and ancestor(folder, 'HEAD', previous)
    # Fetch only changes refs; it leaves all source files and the index alone.
    git(folder, 'fetch', '--prune', '--', remote)
    default_reply = git(folder, 'ls-remote', '--symref', remote, 'HEAD')
    defaults = [line.split()[1].removeprefix('refs/heads/') for line in default_reply.splitlines()
                if line.startswith('ref: refs/heads/') and line.endswith('\tHEAD')]
    if len(defaults) != 1:
        raise Refused(f'{folder}: cannot find the remote default branch. Run {action(folder, "remote", "show", remote)} and check its HEAD branch.')
    default = defaults[0]
    default_ref = f'refs/remotes/{remote}/{default}'
    git(folder, 'rev-parse', '--verify', default_ref)
    upstream = git(folder, 'for-each-ref', '--format=%(upstream)', f'refs/heads/{branch}')
    exists = bool(upstream) and run(folder, 'git', 'rev-parse', '--verify', upstream, check=False).returncode == 0
    covered = branch != default and integrated(folder, 'HEAD', default_ref)
    if covered:
        if not (ancestor(folder, 'HEAD', default_ref) or published_before
                or (exists and ancestor(folder, 'HEAD', upstream))):
            raise Refused(f'{folder}: cannot prove {branch} has no unpublished commits. '
                          f'Run {action(folder, "push", "-u", remote, branch)}, then retry.')
        local = f'refs/heads/{default}'
        if run(folder, 'git', 'rev-parse', '--verify', local, check=False).returncode == 0:
            if not ancestor(folder, local, default_ref):
                raise Refused(f'{folder}: local {default} has commits absent from its remote. '
                              f'Run {action(folder, "log", "--oneline", default_ref + ".." + local)}; publish or reconcile them before retrying.')
        reason = 'its upstream is gone and its changes are already in' if not exists else 'its changes are already in'
        print(f'{branch}: {reason} {default}. Switching safely to {default}.', flush=True)
        return default, default_ref
    if not exists:
        raise Refused(f'{folder}: {branch} has no available upstream and has work absent from {default}. '
                      f'Publish it with {action(folder, "push", "-u", remote, branch)}, then retry. No branch was switched.')
    if not ancestor(folder, 'HEAD', upstream):
        raise Refused(f'{folder}: {branch} has local commits or has diverged from its upstream. '
                      f'Run {action(folder, "log", "--oneline", upstream + "..HEAD")}; '
                      f'publish with {action(folder, "push", remote, branch)} or reconcile the branch before retrying.')
    return branch, upstream


def trust_key(address):
    """Normalize a remote address to host/owner/repo. Credentials, scheme and a .git suffix never reach the key."""
    address = address.strip()
    if '://' in address and not address.startswith('file://'):
        parts = urlsplit(address)
        host = (parts.hostname or '').lower() + (f':{parts.port}' if parts.port else '')
        path = parts.path
    elif '://' not in address and '@' in address.split('/')[0] and ':' in address:
        host, path = address.split('@', 1)[1].split(':', 1)
        host = host.lower()
    else:
        host, path = 'file:', address.removeprefix('file://')
    path = path.strip('/').removesuffix('.git').rstrip('/')
    if host == 'github.com':
        path = path.lower()  # GitHub names ignore case
    return f'{host}/{path}' if host and path else ''


def trust_name(key):
    """The owner/repo shown to people: the last two parts of the address."""
    return '/'.join(key.split('/')[-2:])


def trusted_repositories():
    if not TRUST_FILE.is_file():
        return set()
    lines = (line.strip() for line in TRUST_FILE.read_text().splitlines())
    return {line for line in lines if line and not line.startswith('#')}


def trust(key):
    TRUST_FILE.parent.mkdir(parents=True, exist_ok=True)
    existing = TRUST_FILE.read_text() if TRUST_FILE.is_file() else (
        '# Repositories ./bilibi update may move forward, one per line. Delete a line to stop trusting it.\n')
    TRUST_FILE.write_text(existing + ('' if existing.endswith('\n') else '\n') + key + '\n')


def ask_to_trust(name):
    """True or False for a typed answer; None when nobody can answer."""
    if not (sys.stdin.isatty() and sys.stdout.isatty()):
        return None
    while True:
        try:
            answer = input(f"{name} isn't in Bilibi's official list. Trust it for updates? (y/n) ").strip().lower()
        except EOFError:
            return None
        if answer in ('y', 'yes'):
            return True
        if answer in ('n', 'no', ''):
            return False


def follow_branch(folder, branch):
    before = git(folder, 'rev-parse', 'HEAD')
    target_branch, upstream = branch_plan(folder)
    dirty = changes(folder)
    if dirty:
        refuse_edits(folder, [path for _, path in dirty])
    move_branch(folder, target_branch, upstream)
    after = git(folder, 'rev-parse', 'HEAD')
    return False, (f'{before[:12]} -> {after[:12]} on {target_branch}' if before != after or branch != target_branch
                   else f'already up to date on {target_branch} ({after[:12]})')


def trusted_branch(folder, branch):
    """A branch of a repository outside the official list moves only after the user trusted that repository."""
    remote = run(folder, 'git', 'config', '--get', f'branch.{branch}.remote', check=False).stdout.decode().strip() or 'origin'
    address = run(folder, 'git', 'remote', 'get-url', remote, check=False)
    key = trust_key(address.stdout.decode()) if address.returncode == 0 else ''
    if not key:
        return True, f'on branch {branch}, left as is; it follows no remote repository, so there is nothing to update from'
    name = trust_name(key)
    if key not in trusted_repositories():
        answer = ask_to_trust(name)
        if answer is None:
            return True, (f"{name} isn't in Bilibi's official list and you haven't trusted it, so branch {branch} was left as is. "
                          f'To trust it, run ./bilibi update in a terminal and answer y. '
                          f'Or add the line {key} to {TRUST_FILE}')
        if not answer:
            return True, f"{name} isn't trusted, so branch {branch} was left as is. Run ./bilibi update again to be asked again"
        trust(key)
        print(f'Trusted {name}. It is recorded in {TRUST_FILE}; delete its line there to stop trusting it.', flush=True)
    stopped, outcome = follow_branch(folder, branch)
    return stopped, f'{outcome} (trusted {name}, following its own branch)'


def component_update(folder, facts, platform_version, platform_head, tested):
    """Return the outcome and whether a mount was left alone."""
    if folder.is_symlink() or Path(git(folder, 'rev-parse', '--show-toplevel')).resolve() != folder:
        return True, 'not an independent real checkout; preserve it and follow its mounting guide'
    if remote_org.repair_origin(folder, facts['repository'] if facts else None) is False:
        return True, 'origin uses the former organization, but its repository is not official; check the remote before updating'
    branch = git(folder, 'branch', '--show-current')
    if changes(folder):
        return True, f'local changes; review {action(folder, "status", "--short")} and commit your work before retrying'
    if facts:
        repository = facts['repository']
        origin = git(folder, 'remote', 'get-url', 'origin')
    official = (bool(facts) and repository.startswith('https://github.com/BelimbingProject/')
                and trust_key(origin) == trust_key(repository))
    if not official:
        if branch:
            return trusted_branch(folder, branch)
        return True, ('origin differs from the official catalogue; inspect '
                      f'{action(folder, "remote", "-v")} and reconcile it deliberately' if facts else
                      'not listed in the official catalogue; update this repository deliberately with its owner')
    if branch:
        return follow_branch(folder, branch)
    head = git(folder, 'rev-parse', 'HEAD')
    if head not in {release['commit'] for release in facts['versions'].values()}:
        return True, f'detached at {head[:12]}, which is not an official release commit; left as is'
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
        return True, f'no release supports Platform {platform_version}; ask the component owner to publish a compatible release'
    _, _, version, target = max(choices)
    if head == target:
        return False, f'already at recommended {version} ({head[:12]})'
    installed = max((v for v, release in facts['versions'].items() if release['commit'] == head), key=catalogue.version_tuple)
    if catalogue.version_tuple(version) < catalogue.version_tuple(installed):
        return True, f'recommended {version} is older than installed {installed}; update never downgrades'
    git(folder, 'fetch', '--prune', '--tags', 'origin')
    if run(folder, 'git', 'cat-file', '-e', f'{target}^{{commit}}', check=False).returncode:
        return True, f'recommended commit {target} is unavailable; ask the component owner to publish it'
    git(folder, 'checkout', '--no-overwrite-ignore', '--detach', target)
    return False, f'{head[:12]} -> recommended {version} ({target[:12]})'


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
    outcomes = []
    for layer in ('domains', 'extensions'):
        for folder in sorted((ROOT / 'apps' / layer).iterdir()):
            if not (folder.is_dir() or folder.is_symlink()) or folder.name == 'src' or folder.name.startswith('.'):
                continue
            try:
                stopped, reason = component_update(folder, data.get(layer, {}).get(folder.name), version, head,
                                          data['tested'] if layer == 'domains' else [])
            except (Refused, catalogue.Refused, OSError) as error:
                stopped, reason = True, str(error)
            prefix = f'{folder}: '
            if reason.startswith(prefix):
                reason = reason[len(prefix):]
            outcomes.append((folder, stopped, reason))
    return outcomes


def move_branch(folder, branch, upstream):
    if git(folder, 'branch', '--show-current') != branch:
        if run(folder, 'git', 'show-ref', '--verify', f'refs/heads/{branch}', check=False).returncode:
            git(folder, 'switch', '--no-overwrite-ignore', '--track', '-c', branch, upstream)
        else:
            git(folder, 'switch', '--no-overwrite-ignore', branch)
    git(folder, 'merge', '--ff-only', '--no-overwrite-ignore', upstream)


def update():
    local_catalogue = catalogue.load(ROOT / 'catalogue.json')
    catalogue.check(local_catalogue)
    if remote_org.repair_origin(ROOT, local_catalogue['platform']['repository']) is False:
        raise Refused(f'{ROOT}: origin uses the former organization for a repository outside the official catalogue; '
                      'check the remote before retrying')
    generated_changes()
    branch, upstream = branch_plan(ROOT)
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
        move_branch(ROOT, branch, upstream)
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
    outcomes = update_components(remote)
    subprocess.run([sys.executable, str(ROOT / 'scripts/discover-modules.py')], cwd=ROOT, check=True)
    resolved = subprocess.run(['cargo', 'metadata', '--format-version', '1'], cwd=ROOT, stdout=subprocess.DEVNULL)
    if resolved.returncode:
        raise Refused(f'{ROOT}: Platform updated, but Cargo could not resolve the mounted composition. '
                      'Resolve the diagnostics above, run cargo metadata --format-version 1, then ./bilibi start.')
    print('Composition regenerated for the mounted components.', flush=True)
    if outcomes:
        print('Component update summary:', flush=True)
        for folder, stopped, reason in outcomes:
            print(f'{"Left alone" if stopped else "Updated"}: {folder}: {reason.rstrip(chr(46))}.', flush=True)
    print('Next: ./bilibi start', flush=True)


if __name__ == '__main__':
    try:
        update()
    except (Refused, catalogue.Refused, OSError, ValueError, subprocess.CalledProcessError) as error:
        raise SystemExit(f'bilibi update: {error}') from None
