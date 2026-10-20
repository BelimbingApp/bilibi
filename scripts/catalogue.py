#!/usr/bin/env python3
"""Check and publish `catalogue.json`, the list of Bilibi software (docs/versions.md).

The catalogue names the Platform and each Domain, their released versions,
the Platform versions a Domain version needs, plug-ins, and the tested sets:
a Platform commit and a Domain commit that passed `scripts/domain-ci.sh`
together. Release CI opens a pull request for a new version; the checks on
that pull request accept the file, and publishing the tested sets copies it
to the `catalogue` branch. A person does not edit `catalogue.json` by hand.

  catalogue.py --check [FILE]      refuse a catalogue that is not well formed
  catalogue.py tested --run URL [--platform-version X.Y.Z]
                                   print the tested set of the current pin
                                   (`.local/composition/manifest.txt`) as JSON.
                                   Pass the version when the Release workflow
                                   has not pushed the tag yet.
  catalogue.py publish --from SOURCE --into PUBLISHED [--record SET ...]
                                   write PUBLISHED = SOURCE plus the tested
                                   sets PUBLISHED already holds and each SET
  catalogue.py open-release ...    open a pull request that adds one Domain
                                   or Extension version (the Domain CI does this)
  catalogue.py open-platform-release ...
                                   open a pull request that adds one Platform version
"""
import argparse
import base64
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CATALOGUE = ROOT / 'catalogue.json'
PIN = ROOT / '.local/composition/manifest.txt'
REPO = 'BelimbingApp/bilibi'
REMOTE = f'https://github.com/{REPO}.git'
FORMAT = 1
SEMVER = re.compile(r'\d+\.\d+\.\d+')
SNAKE = re.compile(r'[a-z][a-z0-9]*(_[a-z0-9]+)*')
COMMIT = re.compile(r'[0-9a-f]{40}')
# A Platform requirement is comparators joined by commas: ">=0.1.0, <0.2.0".
COMPARATOR = re.compile(r'(>=|<=|>|<|=)\s*(\d+\.\d+\.\d+)')
DATE = re.compile(r'\d{4}-\d{2}-\d{2}')
URL = re.compile(r'https://\S+')


class Refused(Exception):
    pass


def expect(condition, message):
    if not condition:
        raise Refused(message)


def version_tuple(text):
    return tuple(int(part) for part in text.split('.'))


def parse_requirement(text, where):
    """Each comparator of a requirement as (operator, version)."""
    expect(isinstance(text, str) and text.strip(), f'{where}: platform must be a requirement like ">=0.1.0, <0.2.0"')
    parts = []
    for piece in text.split(','):
        match = COMPARATOR.fullmatch(piece.strip())
        expect(match, f'{where}: cannot read {piece.strip()!r}; write comparators like >=0.1.0 or <0.2.0')
        parts.append((match.group(1), version_tuple(match.group(2))))
    return parts


def satisfies(version, requirement):
    actual = version_tuple(version)
    checks = {'>=': actual.__ge__, '<=': actual.__le__, '>': actual.__gt__, '<': actual.__lt__, '=': actual.__eq__}
    return all(checks[operator](wanted) for operator, wanted in requirement)


def check_versions(versions, where, platform_versions=None):
    expect(isinstance(versions, dict), f'{where}: versions must be an object keyed by version')
    for version, facts in versions.items():
        here = f'{where} {version}'
        expect(SEMVER.fullmatch(version), f'{here}: a version is X.Y.Z')
        expect(isinstance(facts, dict), f'{here}: must be an object')
        expect(DATE.fullmatch(str(facts.get('released', ''))), f'{here}: released must be a date YYYY-MM-DD')
        expect(COMMIT.fullmatch(str(facts.get('commit', ''))), f'{here}: commit must be the full 40-character SHA')
        if platform_versions is not None:
            requirement = parse_requirement(facts.get('platform'), here)
            expect(any(satisfies(candidate, requirement) for candidate in platform_versions),
                   f'{here}: no listed Platform version satisfies {facts["platform"]!r}')
        unknown = set(facts) - {'released', 'commit', 'platform', 'notes'}
        expect(not unknown, f'{here}: unknown keys {sorted(unknown)}')


def check_tested(entry, index, domains):
    where = f'tested[{index}]'
    expect(isinstance(entry, dict), f'{where}: must be an object')
    expect(set(entry) == {'platform', 'domain', 'tested_at', 'run'},
           f'{where}: needs exactly platform, domain, tested_at and run')
    platform, domain = entry['platform'], entry['domain']
    expect(isinstance(platform, dict) and set(platform) == {'version', 'commit'},
           f'{where}: platform needs version and commit')
    expect(isinstance(domain, dict) and set(domain) == {'id', 'version', 'commit'},
           f'{where}: domain needs id, version and commit')
    expect(domain['id'] in domains, f'{where}: Domain {domain["id"]!r} is not in the catalogue; add it under domains')
    for side in (platform, domain):
        expect(COMMIT.fullmatch(str(side['commit'])), f'{where}: commit must be the full 40-character SHA')
        expect(side['version'] is None or SEMVER.fullmatch(str(side['version'])),
               f'{where}: version is X.Y.Z, or null for a commit at no release')
    expect(isinstance(entry['tested_at'], str) and entry['tested_at'].endswith('Z') and DATE.match(entry['tested_at']),
           f'{where}: tested_at is a UTC time like 2026-10-19T08:00:00Z')
    expect(URL.fullmatch(str(entry['run'])), f'{where}: run is the https:// address of the CI run')


def check(catalogue):
    expect(isinstance(catalogue, dict), 'the catalogue is one JSON object')
    expect(catalogue.get('format') == FORMAT, f'format must be {FORMAT}')
    expect({'format', 'platform', 'domains', 'plugins', 'tested'} <= set(catalogue)
           and set(catalogue) <= {'format', 'platform', 'domains', 'extensions', 'plugins', 'tested'},
           'the catalogue has format, platform, domains, plugins, tested and optional extensions')
    platform = catalogue['platform']
    expect(isinstance(platform, dict) and set(platform) == {'name', 'repository', 'versions'},
           'platform has name, repository and versions')
    expect(URL.fullmatch(str(platform['repository'])), 'platform.repository is an https:// address')
    check_versions(platform['versions'], 'platform')
    platform_versions = list(platform['versions'])
    domains = catalogue['domains']
    expect(isinstance(domains, dict), 'domains is an object keyed by Domain ID')
    for kind in ('domains', 'extensions'):
        containers = catalogue.get(kind, {})
        expect(isinstance(containers, dict), f'{kind} is an object keyed by container ID')
        for domain, facts in containers.items():
            where = f'{kind}.{domain}'
            expect(SNAKE.fullmatch(domain), f'{where}: a Domain ID is snake_case, the folder it mounts at')
            expect(isinstance(facts, dict) and set(facts) == {'name', 'description', 'repository', 'modules', 'versions'},
                   f'{where}: needs name, description, repository, modules and versions')
            expect(URL.fullmatch(str(facts['repository'])), f'{where}: repository is an https:// address')
            expect(isinstance(facts['modules'], list) and facts['modules']
                   and all(isinstance(m, str) and SNAKE.fullmatch(m) for m in facts['modules']),
                   f'{where}: modules lists the module folders, snake_case')
            check_versions(facts['versions'], where, platform_versions)
    plugins = catalogue['plugins']
    expect(isinstance(plugins, dict), 'plugins is an object keyed by plug-in ID')
    for plugin, facts in plugins.items():
        where = f'plugins.{plugin}'
        expect(SNAKE.fullmatch(plugin), f'{where}: a plug-in ID is snake_case')
        expect(isinstance(facts, dict) and set(facts) == {'name', 'publisher', 'repository', 'domain', 'versions'},
               f'{where}: needs name, publisher, repository, domain and versions')
        expect(facts['domain'] is None or facts['domain'] in domains,
               f'{where}: domain names a Domain in the catalogue, or is null for a Platform plug-in')
        check_versions(facts['versions'], where, platform_versions)
    tested = catalogue['tested']
    expect(isinstance(tested, list), 'tested is a list')
    seen = set()
    for index, entry in enumerate(tested):
        check_tested(entry, index, domains)
        key = (entry['platform']['commit'], entry['domain']['id'], entry['domain']['commit'])
        expect(key not in seen, f'tested[{index}] repeats an earlier set')
        seen.add(key)


def load(path):
    try:
        return json.loads(Path(path).read_text())
    except (OSError, ValueError) as error:
        raise Refused(f'{path}: {error}') from None


def dump(catalogue):
    return json.dumps(catalogue, indent=2, ensure_ascii=False) + '\n'


def tested_sets(run, platform_version=None):
    """One tested set per pinned container, from the pin the gates just made."""
    try:
        pin = tomllib.loads(PIN.read_text())
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise Refused(f'{PIN.relative_to(ROOT)} is unreadable ({error}); run scripts/discover-modules.py --pin first') from None
    expect(URL.fullmatch(run), '--run is the https:// address of the CI run')
    now = datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
    tagged = pin['platform_tag'][1:] if 'platform_tag' in pin else None
    if platform_version:
        expect(SEMVER.fullmatch(platform_version), '--platform-version is X.Y.Z')
        expect(platform_version == pin.get('platform_version'),
               f'--platform-version {platform_version} but the pin says {pin.get("platform_version")}')
        expect(tagged in (None, platform_version),
               f'the pin is tagged v{tagged} but --platform-version is {platform_version}')
        version = platform_version
    else:
        version = tagged
    platform = {'version': version, 'commit': pin['platform']}
    sets = []
    for name, facts in pin.get('containers', {}).items():
        layer, container = name.split('/', 1)
        if layer != 'domains':
            continue
        sets.append({
            'platform': platform,
            'domain': {'id': container, 'version': facts.get('version'), 'commit': facts['head']},
            'tested_at': now,
            'run': run,
        })
    expect(sets, 'the pin names no Domain; mount one before recording a tested set')
    return sets


def publish(source_path, published_path, records):
    """The source catalogue plus every tested set already published and each record."""
    source = load(source_path)
    check(source)
    tested = {}

    def keep(entry):
        key = (entry['platform']['commit'], entry['domain']['id'], entry['domain']['commit'])
        # The latest run of the same pair is the one that stays.
        if key not in tested or tested[key]['tested_at'] <= entry['tested_at']:
            tested[key] = entry

    for entry in source['tested']:
        keep(entry)
    if Path(published_path).is_file():
        for entry in load(published_path).get('tested', []):
            keep(entry)
    for record in records:
        loaded = load(record)
        for entry in loaded if isinstance(loaded, list) else [loaded]:
            keep(entry)
    published = dict(source)
    published['tested'] = sorted(tested.values(), key=lambda e: (e['tested_at'], e['domain']['id']))
    check(published)
    Path(published_path).parent.mkdir(parents=True, exist_ok=True)
    Path(published_path).write_text(dump(published))
    return len(published['tested'])


def scrub(text, token):
    if token and text:
        return text.replace(token, '***')
    return text or ''


def catalogue_token():
    """The fine-grained token that can open a catalogue pull request.

    In Actions the secret is required, so a release cannot skip the catalogue.
    Outside Actions the caller's own GitHub login is enough.
    """
    token = os.environ.get('BILIBI_CATALOGUE_TOKEN', '').strip()
    if token:
        return token
    if os.environ.get('GITHUB_ACTIONS') == 'true':
        raise Refused('BILIBI_CATALOGUE_TOKEN is not set. Add that Actions secret before the next release '
                      '(docs/versions.md#secret-the-owner-adds).')
    return ''


def github(args, token):
    env = os.environ.copy()
    if token:
        env['GH_TOKEN'] = token
    result = subprocess.run(['gh', *args], capture_output=True, text=True, env=env)
    if result.returncode != 0:
        detail = scrub(result.stderr.strip() or result.stdout.strip(), token)
        raise Refused(detail or 'gh failed')
    return result.stdout


def git(args, token, cwd):
    env = os.environ.copy()
    remote = REMOTE
    if token:
        # Same shape as the tested-set publish job. GitHub masks the secret in logs.
        remote = f'https://x-access-token:{token}@github.com/{REPO}.git'
        env['GIT_CONFIG_COUNT'] = '1'
        env['GIT_CONFIG_KEY_0'] = 'credential.helper'
        env['GIT_CONFIG_VALUE_0'] = ''
    rewritten = [remote if arg == REMOTE else arg for arg in args]
    result = subprocess.run(['git', *rewritten], cwd=cwd, capture_output=True, text=True, env=env)
    if result.returncode != 0:
        detail = scrub(result.stderr.strip() or result.stdout.strip(), token)
        raise Refused(detail or 'git failed')
    return result.stdout


def normal_repository(url):
    return str(url).rstrip('/')


def locate(catalogue, repository):
    """(role, id) entries whose repository URL is `repository`."""
    found = []
    wanted = normal_repository(repository)
    for role in ('domains', 'extensions'):
        for container, facts in catalogue.get(role, {}).items():
            if isinstance(facts, dict) and normal_repository(facts.get('repository', '')) == wanted:
                found.append((role, container))
    return found


def cargo_version(revision, token):
    """The Platform version at one commit, from that commit's Cargo.toml."""
    expect(COMMIT.fullmatch(revision), 'the Platform revision must be the full 40-character SHA')
    payload = json.loads(github(['api', f'repos/{REPO}/contents/Cargo.toml?ref={revision}'], token))
    text = base64.b64decode(payload['content']).decode()
    return tomllib.loads(text)['workspace']['package']['version']


def read_requirement(path):
    """One requirement line from `.github/bilibi-platform`, or empty when the file is absent."""
    if not path:
        return ''
    file = Path(path)
    if not file.is_file():
        return ''
    lines = [line.strip() for line in file.read_text().splitlines()
             if line.strip() and not line.strip().startswith('#')]
    expect(len(lines) == 1, f'{path} must be one requirement, like >=0.1.0, <0.2.0')
    return lines[0]


def requirement_for(catalogue, role, container, platform_text, revision, token):
    """The requirement this release declares, checked against the pinned Platform."""
    if platform_text:
        requirement = platform_text.strip()
    else:
        versions = catalogue[role][container]['versions']
        expect(versions, f'{role}.{container} has no version yet. Write the requirement in .github/bilibi-platform.')
        latest = max(versions, key=version_tuple)
        requirement = versions[latest]['platform']
    parsed = parse_requirement(requirement, f'{role}.{container}')
    pinned = cargo_version(revision, token)
    expect(satisfies(pinned, parsed),
           f'the pinned Platform {pinned} ({revision[:12]}) does not satisfy {requirement}. '
           'Write the new requirement in .github/bilibi-platform.')
    return requirement


def add_version(catalogue, role, container, version, commit, released, platform, repository):
    """Add one Domain or Extension version. Return false when that commit is already listed."""
    expect(role in ('domains', 'extensions'), 'role is domains or extensions')
    expect(SEMVER.fullmatch(version), f'{version} is not X.Y.Z')
    expect(COMMIT.fullmatch(commit), 'commit must be the full 40-character SHA')
    expect(DATE.fullmatch(released), 'released must be a date YYYY-MM-DD')
    containers = catalogue.get(role)
    expect(isinstance(containers, dict) and container in containers, f'{role}.{container} is not in the catalogue')
    facts = containers[container]
    expect(normal_repository(facts['repository']) == normal_repository(repository),
           f'{role}.{container} lists {facts["repository"]}, not {repository}')
    current = facts['versions'].get(version)
    if current is not None:
        expect(current.get('commit') == commit,
               f'{role}.{container} {version} already records commit {current.get("commit")}')
        expect(current.get('platform') == platform,
               f'{role}.{container} {version} already requires {current.get("platform")}')
        return False
    facts['versions'][version] = {'released': released, 'commit': commit, 'platform': platform}
    check(catalogue)
    return True


def add_platform_version(catalogue, version, commit, released):
    """Add one Platform version. Return false when that commit is already listed."""
    expect(SEMVER.fullmatch(version), f'{version} is not X.Y.Z')
    expect(COMMIT.fullmatch(commit), 'commit must be the full 40-character SHA')
    expect(DATE.fullmatch(released), 'released must be a date YYYY-MM-DD')
    current = catalogue['platform']['versions'].get(version)
    if current is not None:
        expect(current.get('commit') == commit, f'platform {version} already records commit {current.get("commit")}')
        return False
    catalogue['platform']['versions'][version] = {'released': released, 'commit': commit}
    check(catalogue)
    return True


def catalogue_pr_body(summary, details):
    lines = [summary, '', *details, '',
             'The catalogue check on this pull request accepts the file. '
             'Merging it lets the next catalogue publish include the version.',
             '',
             '## Upgrade notes for agents',
             '',
             'None.',
             '']
    return '\n'.join(lines)


def open_pull_request(branch, title, body, token):
    """Open the pull request, or print the one already open for this branch.

    GitHub can lag a moment behind the branch push, so a missing head is retried.
    """
    with tempfile.TemporaryDirectory() as directory:
        body_file = Path(directory) / 'body.md'
        body_file.write_text(body)
        last_error = ''
        for attempt in range(3):
            found = json.loads(github(
                ['pr', 'list', '--repo', REPO, '--head', branch, '--state', 'open', '--json', 'number,url'], token))
            if found:
                print(found[0]['url'])
                return found[0]['url']
            try:
                url = github(
                    ['pr', 'create', '--repo', REPO, '--base', 'main', '--head', branch,
                     '--title', title, '--body-file', str(body_file)], token).strip()
            except Refused as refusal:
                last_error = str(refusal)
                if attempt == 2 or 'not found' not in last_error.lower():
                    raise
                time.sleep(2)
                continue
            print(url)
            return url
    raise Refused(last_error or 'could not open the pull request')


def clone_main(token):
    work = Path(tempfile.mkdtemp(prefix='catalogue-release-'))
    try:
        git(['clone', '--quiet', '--branch', 'main', '--single-branch', '--depth', '1', REMOTE, str(work)], token, None)
    except BaseException:
        shutil.rmtree(work, ignore_errors=True)
        raise
    return work


def push_catalogue(work, branch, message, body, token):
    git(['add', 'catalogue.json'], token, work)
    git(['-c', 'user.name=github-actions', '-c', 'user.email=github-actions@github.com',
         'commit', '--quiet', '-m', message], token, work)
    expect(branch.startswith('catalogue-entry/'), f'{branch} is not a catalogue-entry branch')
    ref = f'refs/heads/{branch}'
    earlier = git(['ls-remote', REMOTE, ref], token, work).split()
    lease = [f'--force-with-lease={ref}:{earlier[0]}'] if earlier else []
    git(['push', '--quiet', *lease, REMOTE, f'HEAD:{ref}'], token, work)
    return open_pull_request(branch, message, body, token)


def open_release(role, container, version, commit, repository, released, revision, platform_text):
    """Open the catalogue pull request for one official Domain or Extension release."""
    found = locate(load(CATALOGUE), repository)
    if not found:
        print(f'catalogue: {repository} is not listed, so no pull request was opened')
        return None
    expect(len(found) == 1, f'{repository} is listed more than once')
    listed_role, listed_id = found[0]
    expect((listed_role, listed_id) == (role, container),
           f'{repository} is {listed_role}/{listed_id} in the catalogue, not {role}/{container}')
    token = catalogue_token()
    requirement = requirement_for(load(CATALOGUE), role, container, platform_text, revision, token)
    work = clone_main(token)
    try:
        path = work / 'catalogue.json'
        fresh = load(path)
        if not add_version(fresh, role, container, version, commit, released, requirement, repository):
            print(f'catalogue: {role}.{container} {version} is already listed')
            return None
        path.write_text(dump(fresh))
        # `catalogue` is already the published branch, so a slash under it cannot be a ref.
        branch = f'catalogue-entry/{role}/{container}-v{version}'
        title = f'Add {container} {version} to the catalogue'
        body = catalogue_pr_body(
            f'This adds {container} {version} to the catalogue.',
            [f'- Repository: {repository}',
             f'- Commit: `{commit}`',
             f'- Released: {released}',
             f'- Platform requirement: `{requirement}`'])
        return push_catalogue(work, branch, title, body, token)
    finally:
        shutil.rmtree(work, ignore_errors=True)


def open_platform_release(version, commit, released):
    """Open the catalogue pull request for one Platform release."""
    token = catalogue_token()
    work = clone_main(token)
    try:
        path = work / 'catalogue.json'
        fresh = load(path)
        if not add_platform_version(fresh, version, commit, released):
            print(f'catalogue: platform {version} is already listed')
            return None
        path.write_text(dump(fresh))
        branch = f'catalogue-entry/platform-v{version}'
        title = f'Add Bilibi {version} to the catalogue'
        body = catalogue_pr_body(
            f'This adds Bilibi {version} to the catalogue.',
            [f'- Commit: `{commit}`', f'- Released: {released}'])
        return push_catalogue(work, branch, title, body, token)
    finally:
        shutil.rmtree(work, ignore_errors=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest='command')
    parser.add_argument('--check', nargs='?', const=str(CATALOGUE), metavar='FILE',
                        help='refuse a catalogue that is not well formed (default: catalogue.json)')
    tested = commands.add_parser('tested', help='print the tested sets of the current pin as JSON')
    tested.add_argument('--run', required=True, help='the https:// address of the CI run')
    tested.add_argument('--platform-version', help='record this version when the commit is not tagged yet')
    publishing = commands.add_parser('publish', help='merge tested sets into the published catalogue')
    publishing.add_argument('--from', dest='source', required=True, help='the catalogue people edit (main)')
    publishing.add_argument('--into', dest='published', required=True, help='the published catalogue to write')
    publishing.add_argument('--record', nargs='*', default=[], help='tested-set files from `tested`')
    opening = commands.add_parser('open-release', help='open a pull request for a Domain or Extension release')
    opening.add_argument('--role', required=True, choices=('domains', 'extensions'))
    opening.add_argument('--id', required=True, help='the catalogue id, the folder the container mounts at')
    opening.add_argument('--version', required=True, help='X.Y.Z from the vX.Y.Z tag')
    opening.add_argument('--commit', required=True, help='the full commit the tag points at')
    opening.add_argument('--repository', required=True, help='https://github.com/org/name')
    opening.add_argument('--released', required=True, help='YYYY-MM-DD')
    opening.add_argument('--revision', required=True, help='the pinned Platform commit (.github/bilibi-revision)')
    opening.add_argument('--platform', default='', help='the Platform requirement; omit to read --platform-file or the previous one')
    opening.add_argument('--platform-file', default='', help='a file with one requirement, usually .github/bilibi-platform')
    platform = commands.add_parser('open-platform-release', help='open a pull request for a Platform release')
    platform.add_argument('--version', required=True)
    platform.add_argument('--commit', required=True)
    platform.add_argument('--released', required=True)
    editing = commands.add_parser('add-version', help='add one version to a catalogue file, without opening a pull request')
    editing.add_argument('--file', required=True)
    editing.add_argument('--role', required=True, choices=('domains', 'extensions', 'platform'))
    editing.add_argument('--id', default='', help='Domain or Extension id; omit for the Platform')
    editing.add_argument('--version', required=True)
    editing.add_argument('--commit', required=True)
    editing.add_argument('--released', required=True)
    editing.add_argument('--platform', default='', help='requirement for a Domain or Extension version')
    editing.add_argument('--repository', default='', help='https address; required for a Domain or Extension')
    args = parser.parse_args()
    try:
        if args.command == 'tested':
            print(dump(tested_sets(args.run, args.platform_version)), end='')
        elif args.command == 'publish':
            count = publish(args.source, args.published, args.record)
            print(f'published {args.published} with {count} tested set(s)')
        elif args.command == 'open-release':
            text = read_requirement(args.platform_file) or args.platform
            open_release(args.role, args.id, args.version, args.commit, args.repository,
                         args.released, args.revision, text)
        elif args.command == 'open-platform-release':
            open_platform_release(args.version, args.commit, args.released)
        elif args.command == 'add-version':
            catalogue = load(args.file)
            if args.role == 'platform':
                added = add_platform_version(catalogue, args.version, args.commit, args.released)
            else:
                added = add_version(catalogue, args.role, args.id, args.version, args.commit,
                                    args.released, args.platform, args.repository)
            if added:
                Path(args.file).write_text(dump(catalogue))
                print(f'added {args.role} {args.version}')
            else:
                print(f'{args.role} {args.version} is already listed')
        elif args.check:
            check(load(args.check))
            print(f'{args.check} is well formed')
        else:
            parser.print_help()
            sys.exit(2)
    except Refused as refusal:
        raise SystemExit(f'catalogue: {refusal}') from None


if __name__ == '__main__':
    main()
