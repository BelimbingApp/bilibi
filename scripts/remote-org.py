"""One-time repair for exact GitHub origins under Bilibi's former organization."""
from pathlib import Path
import re
import subprocess


OLD_ORG_ORIGIN = re.compile(
    r'^(?:https://github\.com/|git@github\.com:|ssh://git@github\.com/)'
    r'BelimbingApp/(?P<repository>[A-Za-z0-9][A-Za-z0-9_.-]*)(?:\.git)?$'
)
OFFICIAL_ORIGIN = re.compile(
    r'^https://github\.com/BelimbingProject/(?P<repository>[A-Za-z0-9][A-Za-z0-9_.-]*)(?:\.git)?$'
)


def repair_origin(folder, official_repository):
    """Repoint an official old origin; return False for an old, non-official one."""
    folder = Path(folder)
    result = subprocess.run(['git', 'config', '--get', 'remote.origin.url'], cwd=folder,
                            capture_output=True, text=True)
    if result.returncode:
        return None
    address = result.stdout.rstrip('\n')
    match = OLD_ORG_ORIGIN.fullmatch(address)
    if not match:
        return None
    repository = match.group('repository').removesuffix('.git')
    official = OFFICIAL_ORIGIN.fullmatch(official_repository or '')
    if not official or official.group('repository').removesuffix('.git').casefold() != repository.casefold():
        print(f'Warning: {folder}: origin names github.com/BelimbingApp/{repository}, but BelimbingApp '
              'is no longer ours; check this remote before updating.', flush=True)
        return False
    replacement = address.replace('/BelimbingApp/', '/BelimbingProject/', 1)
    replacement = replacement.replace(':BelimbingApp/', ':BelimbingProject/', 1)
    subprocess.run(['git', 'remote', 'set-url', 'origin', replacement], cwd=folder, check=True)
    print(f'Repointed origin for {folder}: BelimbingApp/{repository} -> BelimbingProject/{repository}.', flush=True)
    return True
