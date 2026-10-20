#!/usr/bin/env python3
"""Check, refresh and publish `catalogue.json`, the list of Bilibi software (docs/versions.md).

The catalogue names the Platform and each Domain, their released versions,
the Platform versions a Domain version needs, plug-ins, and the tested sets:
a Platform commit and a Domain commit that passed `scripts/domain-ci.sh`
together. Bilibi fetches releases itself: a scheduled workflow reads the
`vX.Y.Z` tags of every repository the catalogue lists and writes the new
versions to the `catalogue` branch with its own token. `main` keeps the list of
official software and is only changed by pull request. A person does not edit
the versions by hand.

  catalogue.py --check [FILE]      refuse a catalogue that is not well formed
  catalogue.py tested --run URL [--platform-version X.Y.Z]
                                   print the tested set of the current pin
                                   (`.local/composition/manifest.txt`) as JSON.
                                   Pass the version when the Release workflow
                                   has not pushed the tag yet.
  catalogue.py publish --from SOURCE --into PUBLISHED [--record SET ...]
                                   write PUBLISHED = SOURCE plus the versions
                                   and tested sets PUBLISHED already holds and
                                   each SET
  catalogue.py refresh --from SOURCE --into PUBLISHED [--platform-version X.Y.Z]
                                   publish, then add every release tag of a
                                   listed repository that is not listed yet
"""
import argparse
import base64
import copy
import json
import os
import re
import subprocess
import sys
import tomllib
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CATALOGUE = ROOT / 'catalogue.json'
PIN = ROOT / '.local/composition/manifest.txt'
REPO = 'BelimbingApp/bilibi'
FORMAT = 1
SEMVER = re.compile(r'\d+\.\d+\.\d+')
SNAKE = re.compile(r'[a-z][a-z0-9]*(_[a-z0-9]+)*')
COMMIT = re.compile(r'[0-9a-f]{40}')
# A Platform requirement is comparators joined by commas: ">=0.1.0, <0.2.0".
COMPARATOR = re.compile(r'(>=|<=|>|<|=)\s*(\d+\.\d+\.\d+)')
DATE = re.compile(r'\d{4}-\d{2}-\d{2}')
URL = re.compile(r'https://\S+')
GITHUB_REPOSITORY = re.compile(r'https://github\.com/([\w.-]+/[\w.-]+?)(?:\.git)?/?')
TAG_REF = re.compile(r'refs/tags/v(\d+\.\d+\.\d+)')
# The job of the CI template (docs/domain-ci/ci.yml) that runs the mounted gates.
GATES_CHECK = 'Mounted in Bilibi'


class Refused(Exception):
    pass


class Pending(Refused):
    """Not an error: the release cannot be listed yet, so try again at the next run."""


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


def normal_repository(url):
    return str(url).rstrip('/')


def keep_versions(source, earlier):
    """List in `source` the versions `earlier` holds. A version `source` lists itself wins."""
    for role in ('domains', 'extensions'):
        for container, facts in source.get(role, {}).items():
            before = earlier.get(role, {}).get(container)
            if isinstance(before, dict) and isinstance(before.get('versions'), dict):
                for version, release in before['versions'].items():
                    facts['versions'].setdefault(version, release)
    before = earlier.get('platform', {}).get('versions')
    if isinstance(before, dict):
        for version, release in before.items():
            source['platform']['versions'].setdefault(version, release)


def sort_versions(catalogue):
    containers = [catalogue['platform']]
    for role in ('domains', 'extensions', 'plugins'):
        containers.extend(catalogue.get(role, {}).values())
    for facts in containers:
        facts['versions'] = dict(sorted(facts['versions'].items(), key=lambda item: version_tuple(item[0])))


def combined(source_path, published_path, records):
    """SOURCE plus the versions and tested sets PUBLISHED already holds and each record."""
    catalogue = load(source_path)
    check(catalogue)
    earlier = load(published_path) if Path(published_path).is_file() else {}
    expect(isinstance(earlier, dict), f'{published_path}: the catalogue is one JSON object')
    keep_versions(catalogue, earlier)
    tested = {}

    def keep(entry):
        key = (entry['platform']['commit'], entry['domain']['id'], entry['domain']['commit'])
        # The latest run of the same pair is the one that stays.
        if key not in tested or tested[key]['tested_at'] <= entry['tested_at']:
            tested[key] = entry

    for entry in catalogue['tested']:
        keep(entry)
    for entry in earlier.get('tested', []):
        keep(entry)
    for record in records:
        loaded = load(record)
        for entry in loaded if isinstance(loaded, list) else [loaded]:
            keep(entry)
    catalogue['tested'] = sorted(tested.values(), key=lambda e: (e['tested_at'], e['domain']['id']))
    check(catalogue)
    sort_versions(catalogue)
    return catalogue


def write_published(path, catalogue):
    """Write the published catalogue, leaving the file alone when nothing changed."""
    text = dump(catalogue)
    file = Path(path)
    if file.is_file() and file.read_text() == text:
        return
    file.parent.mkdir(parents=True, exist_ok=True)
    file.write_text(text)


def publish(source_path, published_path, records):
    """The source catalogue plus every version and tested set already published and each record."""
    published = combined(source_path, published_path, records)
    write_published(published_path, published)
    return len(published['tested'])


def github(args):
    result = subprocess.run(['gh', *args], capture_output=True, text=True)
    if result.returncode != 0:
        raise Refused((result.stderr.strip() or result.stdout.strip()) or 'gh failed')
    return result.stdout


def repository_slug(url):
    match = GITHUB_REPOSITORY.fullmatch(str(url))
    expect(match, f'{url} is not a github.com repository address')
    return match.group(1)


def release_tags(slug):
    """Each `vX.Y.Z` tag of a public repository as {version: (tag object, commit)}."""
    env = {**os.environ, 'GIT_TERMINAL_PROMPT': '0'}
    result = subprocess.run(['git', 'ls-remote', '--tags', f'https://github.com/{slug}.git'],
                            capture_output=True, text=True, env=env)
    if result.returncode != 0:
        raise Refused(f'cannot list the tags of {slug}: {result.stderr.strip() or "git failed"}')
    objects, peeled = {}, {}
    for line in result.stdout.splitlines():
        sha, ref = line.split('\t', 1)
        if ref.endswith('^{}'):
            peeled[ref[:-3]] = sha
        else:
            objects[ref] = sha
    tags = {}
    for ref, sha in objects.items():
        match = TAG_REF.fullmatch(ref)
        if match:
            # An annotated tag has a tag object; the peeled line is the commit it points at.
            tags[match.group(1)] = (sha, peeled.get(ref, sha))
    return tags


def remote_file(slug, path, ref):
    """A text file of a public repository at one commit, or None when it is not there."""
    result = subprocess.run(['gh', 'api', f'repos/{slug}/contents/{path}?ref={ref}', '--jq', '.content'],
                            capture_output=True, text=True)
    if result.returncode != 0:
        if 'Not Found' in result.stderr or '404' in result.stderr:
            return None
        raise Refused(result.stderr.strip() or f'cannot read {path} of {slug}')
    return base64.b64decode(result.stdout).decode()


def released_on(slug, tag_object, commit):
    """The UTC date the tag was made, or the commit's date for a tag with no tag object."""
    if tag_object != commit:
        stamp = json.loads(github(['api', f'repos/{slug}/git/tags/{tag_object}']))['tagger']['date']
    else:
        stamp = json.loads(github(['api', f'repos/{slug}/commits/{commit}']))['commit']['committer']['date']
    return datetime.fromisoformat(stamp.replace('Z', '+00:00')).astimezone(timezone.utc).date().isoformat()


def require_gates(slug, commit):
    """The repository's own CI passed the mounted gates on the tagged commit."""
    runs = json.loads(github(['api', f'repos/{slug}/commits/{commit}/check-runs?per_page=100']))['check_runs']
    gates = [run for run in runs if run['name'] == GATES_CHECK]
    if any(run['conclusion'] == 'success' for run in gates):
        return
    if not gates or any(run['status'] != 'completed' for run in gates):
        raise Pending(f'the "{GATES_CHECK}" check has not finished on {commit[:12]}; it is tried again at the next run')
    raise Refused(f'the "{GATES_CHECK}" check did not pass on {commit[:12]}. Release a commit whose CI is green')


def requirement_line(text, where):
    """The one requirement line of `.github/bilibi-platform`."""
    lines = [line.strip() for line in text.splitlines() if line.strip() and not line.strip().startswith('#')]
    expect(len(lines) == 1, f'{where}: .github/bilibi-platform must be one requirement, like >=0.1.0, <0.2.0')
    return lines[0]


def cargo_version(revision):
    """The Platform version at one commit, from that commit's Cargo.toml."""
    expect(COMMIT.fullmatch(revision), 'the Platform revision must be the full 40-character SHA')
    payload = json.loads(github(['api', f'repos/{REPO}/contents/Cargo.toml?ref={revision}']))
    text = base64.b64decode(payload['content']).decode()
    return tomllib.loads(text)['workspace']['package']['version']


def requirement_at(facts, version, slug, commit, where):
    """The requirement a release declares, checked against the Platform it was pinned to.

    `.github/bilibi-platform` at the tag, or the nearest earlier version's requirement.
    """
    text = remote_file(slug, '.github/bilibi-platform', commit)
    if text is not None:
        requirement = requirement_line(text, where)
    else:
        earlier = [listed for listed in facts['versions'] if version_tuple(listed) < version_tuple(version)]
        expect(earlier, f'{where} has no earlier version. Write the requirement in .github/bilibi-platform')
        requirement = facts['versions'][max(earlier, key=version_tuple)]['platform']
    parsed = parse_requirement(requirement, where)
    revision = remote_file(slug, '.github/bilibi-revision', commit)
    expect(revision is not None, f'{where}: .github/bilibi-revision is missing at {commit[:12]}')
    revision = revision.strip()
    pinned = cargo_version(revision)
    expect(satisfies(pinned, parsed),
           f'the pinned Platform {pinned} ({revision[:12]}) does not satisfy {requirement}. '
           'Write the new requirement in .github/bilibi-platform')
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


def add_release(catalogue, role, container, version, tag_object, commit):
    """List one release of a Domain or Extension that its own CI passed."""
    facts = catalogue[role][container]
    slug = repository_slug(facts['repository'])
    where = f'{role}.{container} {version}'
    require_gates(slug, commit)
    requirement = requirement_at(facts, version, slug, commit, where)
    released = released_on(slug, tag_object, commit)
    return add_version(catalogue, role, container, version, commit, released, requirement, facts['repository'])


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


def add_platform_release(catalogue, version):
    """List one Platform release. The Release workflow made the tag after Official compatibility passed."""
    tags = release_tags(REPO)
    expect(version in tags, f'{REPO} has no tag v{version}')
    tag_object, commit = tags[version]
    expect(cargo_version(commit) == version, f'Cargo.toml at v{version} does not say {version}')
    return add_platform_version(catalogue, version, commit, released_on(REPO, tag_object, commit))


def refresh(source_path, published_path, platform_version):
    """Publish, then list every release tag the catalogue does not hold yet.

    Returns (added, waiting, refused). A release that cannot be listed does not
    stop the others.
    """
    catalogue = combined(source_path, published_path, [])
    added, waiting, refused = [], [], []

    def attempt(label, change):
        nonlocal catalogue
        trial = copy.deepcopy(catalogue)
        try:
            listed = change(trial)
        except Pending as pending:
            waiting.append(f'{label}: {pending}')
        except Refused as refusal:
            refused.append(f'{label}: {refusal}')
        else:
            catalogue = trial
            if listed:
                added.append(label)

    def moved(label, listed, tags):
        for version, release in listed.items():
            if version in tags and tags[version][1] != release['commit']:
                refused.append(f'{label} {version}: the tag now points at {tags[version][1][:12]} '
                               f'but the catalogue lists {release["commit"][:12]}. Release the next number instead of moving a tag')

    if platform_version:
        expect(SEMVER.fullmatch(platform_version), '--platform-version is X.Y.Z')
        attempt(f'platform {platform_version}', lambda trial: add_platform_release(trial, platform_version))
    try:
        moved('platform', catalogue['platform']['versions'], release_tags(REPO))
    except Refused as refusal:
        refused.append(f'platform: {refusal}')
    for role in ('domains', 'extensions'):
        for container in list(catalogue.get(role, {})):
            label = f'{role}.{container}'
            try:
                tags = release_tags(repository_slug(catalogue[role][container]['repository']))
            except Refused as refusal:
                refused.append(f'{label}: {refusal}')
                continue
            moved(label, catalogue[role][container]['versions'], tags)
            for version in sorted(set(tags) - set(catalogue[role][container]['versions']), key=version_tuple):
                tag_object, commit = tags[version]
                attempt(f'{label} {version}',
                        lambda trial: add_release(trial, role, container, version, tag_object, commit))
    sort_versions(catalogue)
    write_published(published_path, catalogue)
    return added, waiting, refused


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
    refreshing = commands.add_parser('refresh', help='publish, then list the release tags that are not listed yet')
    refreshing.add_argument('--from', dest='source', required=True, help='the catalogue people edit (main)')
    refreshing.add_argument('--into', dest='published', required=True, help='the published catalogue to write')
    refreshing.add_argument('--platform-version', help='also list this Platform release, made by the Release workflow')
    args = parser.parse_args()
    try:
        if args.command == 'tested':
            print(dump(tested_sets(args.run, args.platform_version)), end='')
        elif args.command == 'publish':
            count = publish(args.source, args.published, args.record)
            print(f'published {args.published} with {count} tested set(s)')
        elif args.command == 'refresh':
            added, waiting, refused = refresh(args.source, args.published, args.platform_version)
            actions = os.environ.get('GITHUB_ACTIONS') == 'true'
            for label in added:
                print(f'listed {label}')
            for message in waiting:
                print(f'::warning::{message}' if actions else f'waiting: {message}')
            for message in refused:
                print(f'::error::{message}' if actions else f'refused: {message}')
            print(f'{args.published}: {len(added)} listed, {len(waiting)} waiting, {len(refused)} refused')
            if refused:
                sys.exit(1)
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
