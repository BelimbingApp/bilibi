#!/usr/bin/env python3
"""Check and publish `catalogue.json`, the list of Bilibi software (docs/versions.md).

The catalogue names the Platform and each Domain, their released versions,
the Platform versions a Domain version needs, plug-ins, and the tested sets:
a Platform commit and a Domain commit that passed `scripts/domain-ci.sh`
together. People edit the first part on `main`; CI records the tested sets
and publishes the whole file on the `catalogue` branch.

  catalogue.py --check [FILE]      refuse a catalogue that is not well formed
  catalogue.py tested --run URL    print the tested set of the current pin
                                   (`.local/composition/manifest.txt`) as JSON
  catalogue.py publish --from SOURCE --into PUBLISHED [--record SET ...]
                                   write PUBLISHED = SOURCE plus the tested
                                   sets PUBLISHED already holds and each SET
"""
import argparse
import json
import re
import sys
import tomllib
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CATALOGUE = ROOT / 'catalogue.json'
PIN = ROOT / '.local/composition/manifest.txt'
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
    expect(set(catalogue) == {'format', 'platform', 'domains', 'plugins', 'tested'},
           'the catalogue has exactly format, platform, domains, plugins and tested')
    platform = catalogue['platform']
    expect(isinstance(platform, dict) and set(platform) == {'name', 'repository', 'versions'},
           'platform has name, repository and versions')
    expect(URL.fullmatch(str(platform['repository'])), 'platform.repository is an https:// address')
    check_versions(platform['versions'], 'platform')
    platform_versions = list(platform['versions'])
    domains = catalogue['domains']
    expect(isinstance(domains, dict), 'domains is an object keyed by Domain ID')
    for domain, facts in domains.items():
        where = f'domains.{domain}'
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


def tested_sets(run):
    """One tested set per pinned container, from the pin the gates just made."""
    try:
        pin = tomllib.loads(PIN.read_text())
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise Refused(f'{PIN.relative_to(ROOT)} is unreadable ({error}); run scripts/discover-modules.py --pin first') from None
    expect(URL.fullmatch(run), '--run is the https:// address of the CI run')
    now = datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
    platform = {'version': pin['platform_tag'][1:] if 'platform_tag' in pin else None, 'commit': pin['platform']}
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


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest='command')
    parser.add_argument('--check', nargs='?', const=str(CATALOGUE), metavar='FILE',
                        help='refuse a catalogue that is not well formed (default: catalogue.json)')
    tested = commands.add_parser('tested', help='print the tested sets of the current pin as JSON')
    tested.add_argument('--run', required=True, help='the https:// address of the CI run')
    publishing = commands.add_parser('publish', help='merge tested sets into the published catalogue')
    publishing.add_argument('--from', dest='source', required=True, help='the catalogue people edit (main)')
    publishing.add_argument('--into', dest='published', required=True, help='the published catalogue to write')
    publishing.add_argument('--record', nargs='*', default=[], help='tested-set files from `tested`')
    args = parser.parse_args()
    try:
        if args.command == 'tested':
            print(dump(tested_sets(args.run)), end='')
        elif args.command == 'publish':
            count = publish(args.source, args.published, args.record)
            print(f'published {args.published} with {count} tested set(s)')
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
