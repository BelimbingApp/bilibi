#!/usr/bin/env python3
"""Gate Platform releases and gather upgrade notes (docs/versions.md).

Platform-only automation; there is no Bilimbi module counterpart.

  release.py ready            refuse unless this commit may be released
  release.py require-notes    refuse a pull request that omits upgrade notes
  release.py gather           write docs/upgrade-notes/vX.Y.Z.md from merged pull requests
  release.py publish          tag and publish after Official compatibility has passed
"""
import json
import os
import re
import subprocess
import sys
import tomllib
from datetime import datetime, timedelta, timezone
from pathlib import Path

from catalogue import REPO, Refused, expect, github

ROOT = Path(__file__).resolve().parent.parent
SEMVER = re.compile(r'\d+\.\d+\.\d+')
HEADING = re.compile(r'^(#{1,3})\s+Upgrade notes for agents\s*$', re.MULTILINE)


def extract_notes(body):
    """The text under an Upgrade notes for agents heading, without HTML comments."""
    if not body:
        return None
    match = HEADING.search(body)
    if not match:
        return None
    level = len(match.group(1))
    lines = []
    for line in body[match.end():].splitlines():
        heading = re.match(r'^(#+)\s+', line)
        if heading and len(heading.group(1)) <= level:
            break
        lines.append(line)
    text = re.sub(r'<!--.*?-->', '', '\n'.join(lines), flags=re.DOTALL).strip()
    return text or None


def workspace_version():
    version = tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']['package']['version']
    expect(SEMVER.fullmatch(version), f'Cargo.toml version {version} is not X.Y.Z')
    return version


def notes_file(version):
    return ROOT / 'docs' / 'upgrade-notes' / f'v{version}.md'


def write_output(name, value):
    path = os.environ.get('GITHUB_OUTPUT')
    if path:
        with open(path, 'a', encoding='utf-8') as output:
            output.write(f'{name}={value}\n')


def git_out(args, check=True):
    result = subprocess.run(['git', *args], cwd=ROOT, capture_output=True, text=True)
    if check and result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        raise Refused(detail or 'git failed')
    return result


def require_notes_file(version):
    path = notes_file(version)
    relative = path.relative_to(ROOT)
    expect(path.is_file(), f'{relative} is missing. Run python3 scripts/release.py gather and commit it.')
    expect(extract_notes(path.read_text()),
           f'{relative} needs a non-empty "Upgrade notes for agents" section.')
    return path


def assert_clean():
    status = git_out(['status', '--porcelain'])
    expect(not status.stdout.strip(), 'Commit the upgrade notes first. The release tags a clean commit.')


def assert_tag_free(tag, sha):
    """Refuse when the tag exists on another commit. The same commit may retry the GitHub release."""
    git_out(['fetch', '--quiet', '--tags', 'origin'], check=False)
    found = git_out(['rev-parse', '-q', '--verify', f'refs/tags/{tag}^{{}}'], check=False)
    if found.returncode != 0:
        return False
    pointed = found.stdout.strip()
    expect(pointed == sha, f'{tag} already points at {pointed[:12]}. Release the next number instead of moving the tag.')
    return True


def ready():
    """Print the version when this commit is allowed to start the release gate."""
    ref = os.environ.get('GITHUB_REF', '')
    if ref:
        expect(ref == 'refs/heads/main', 'Run the Release workflow from main (docs/versions.md).')
    version = workspace_version()
    require_notes_file(version)
    assert_clean()
    sha = git_out(['rev-parse', 'HEAD']).stdout.strip()
    assert_tag_free(f'v{version}', sha)
    write_output('version', version)
    print(version)


def require_notes():
    """Refuse a pull request whose body has no upgrade notes."""
    if os.environ.get('GITHUB_EVENT_NAME') != 'pull_request':
        print('not a pull request')
        return
    event_path = os.environ.get('GITHUB_EVENT_PATH', '')
    expect(event_path, 'GITHUB_EVENT_PATH is not set')
    event = json.loads(Path(event_path).read_text())
    body = (event.get('pull_request') or {}).get('body') or ''
    expect(extract_notes(body),
           'Add an "Upgrade notes for agents" section to the pull request. '
           'Say what a Domain or Extension owner must change, and how, or write None. '
           'The heading is in the pull request template (docs/versions.md).')
    print('upgrade notes present')


def previous_tag(version):
    listed = git_out(['tag', '--list', 'v[0-9]*.[0-9]*.[0-9]*', '--sort=-v:refname']).stdout.split()
    tags = [tag for tag in listed if SEMVER.fullmatch(tag[1:]) and tag != f'v{version}']
    expect(tags, 'No earlier vX.Y.Z tag. Write docs/upgrade-notes by hand for the first release.')
    return tags[0]


def utc(stamp):
    return datetime.fromisoformat(stamp).astimezone(timezone.utc)


def merged_pulls(after):
    """Merged pull requests whose merge time is after `after` (ISO-8601 with an offset)."""
    since = utc(after)
    day = (since - timedelta(days=1)).date().isoformat()
    raw = github([
        'pr', 'list', '--repo', REPO, '--state', 'merged', '--base', 'main', '--limit', '200',
        '--search', f'merged:>={day}', '--json', 'number,title,body,mergedAt,url',
    ])
    pulls = [pull for pull in json.loads(raw) if pull.get('mergedAt') and utc(pull['mergedAt']) > since]
    pulls.sort(key=lambda pull: pull['number'])
    return pulls


def gather():
    version = workspace_version()
    previous = previous_tag(version)
    after = git_out(['log', '-1', '--format=%cI', previous]).stdout.strip()
    pulls = merged_pulls(after)
    lines = [
        '# Upgrade notes for agents',
        '',
        f'Bilibi v{version}. These notes tell an agent what to change in a private Domain or Extension. '
        f'They come from pull requests merged after {previous}.',
        '',
    ]
    if not pulls:
        lines.extend([f'No pull requests were merged after {previous}.', ''])
    for pull in pulls:
        notes = extract_notes(pull.get('body') or '')
        title = ' '.join(pull['title'].split())
        lines.extend([
            f'## Pull request {pull["number"]}: {title}',
            '',
            pull['url'],
            '',
            notes or 'No upgrade notes were written on this pull request.',
            '',
        ])
    path = notes_file(version)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text('\n'.join(lines))
    print(path.relative_to(ROOT))


def require_compatibility():
    """This workflow run's Official compatibility job passed for the commit we are about to tag."""
    run_id = os.environ.get('GITHUB_RUN_ID', '')
    repository = os.environ.get('GITHUB_REPOSITORY', '')
    expect(run_id and repository,
           'Publish runs from the Release workflow, after Official compatibility passes for this commit '
           '(docs/versions.md).')
    raw = github([
        'api', f'repos/{repository}/actions/runs/{run_id}/jobs?per_page=100',
        '--jq', '[.jobs[] | {name,conclusion}]',
    ])
    jobs = json.loads(raw)
    matched = [job for job in jobs if 'Official compatibility' in (job.get('name') or '')]
    names = ', '.join(job.get('name') or '' for job in jobs) or '(none)'
    expect(matched, f'this run has no Official compatibility job ({names})')
    failed = [job['name'] for job in matched if job.get('conclusion') != 'success']
    expect(not failed, 'Official compatibility did not pass: ' + ', '.join(failed))


def ensure_release(tag, sha, notes):
    view = subprocess.run(['gh', 'release', 'view', tag, '--repo', REPO], capture_output=True, text=True)
    if view.returncode != 0:
        github(['release', 'create', tag, '--repo', REPO, '--title', f'Bilibi {tag}',
                '--notes-file', str(notes), '--target', sha, '--verify-tag'])
        return
    github(['release', 'edit', tag, '--repo', REPO, '--notes-file', str(notes)])


def publish():
    version = workspace_version()
    notes = require_notes_file(version)
    assert_clean()
    require_compatibility()
    sha = git_out(['rev-parse', 'HEAD']).stdout.strip()
    expected = os.environ.get('GITHUB_SHA', '')
    if expected:
        expect(sha == expected, 'HEAD is not the commit Official compatibility checked.')
    tag = f'v{version}'
    if not assert_tag_free(tag, sha):
        git_out(['-c', 'user.name=github-actions', '-c', 'user.email=github-actions@github.com',
                 'tag', '-a', tag, sha, '-m', f'Bilibi {tag}'])
        git_out(['push', 'origin', f'refs/tags/{tag}'])
    ensure_release(tag, sha, notes)
    print(f'published {tag}')


def main(argv):
    commands = {
        'ready': ready,
        'require-notes': require_notes,
        'gather': gather,
        'publish': publish,
    }
    if len(argv) != 2 or argv[1] not in commands:
        raise SystemExit('usage: release.py ready|require-notes|gather|publish')
    try:
        commands[argv[1]]()
    except Refused as refusal:
        raise SystemExit(f'release: {refusal}') from None


if __name__ == '__main__':
    main(sys.argv)
