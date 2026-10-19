#!/usr/bin/env python3
"""Generate the four Cargo composition crates from deep-module manifests.

Cargo resolves dependencies before build.rs runs, so composition manifests must
be refreshed before Cargo. Runtime services and SQL are inventory registrations
owned by each module; there is no second runtime module list here.

Base and Core modules sit one level deep (`apps/<layer>/<module>`). Domain and
Extension modules sit two levels deep inside a mounted container checkout
(`apps/<layer>/<container>/<module>`) that declares itself in
`bilibi.container.toml`. The committed composition is always the unmounted one,
so `--check` also refuses a staged or committed mounted composition or
`Cargo.lock`.

A company build mounts Domain and Extension containers at chosen revisions.
`--pin` records the Platform and container HEADs, the version each is at (its
`vX.Y.Z` tag, see docs/versions.md) and the resolved `Cargo.lock` under
`.local/composition/`; `apps/web/build.rs` compiles that
manifest into the program. `--restore` rebuilds exactly that composition
(containers at the pinned HEADs, the pinned `Cargo.lock`) so that
`cargo build --release --locked` is reproducible.
"""
import argparse
import hashlib
import re
import shutil
import subprocess
from pathlib import Path
import tomllib

ROOT = Path(__file__).resolve().parent.parent
LAYERS = ('base', 'core', 'domains', 'extensions')
CONTAINER_LAYERS = {'domains': 'domain', 'extensions': 'extension'}
CONTAINER_FILE = 'bilibi.container.toml'
RESERVED_IDS = {'base', 'core', 'web', 'src'}
SNAKE = re.compile(r'[a-z][a-z0-9]*(_[a-z0-9]+)*')
# Path packages the Platform itself owns besides discovered Base and Core modules.
PLATFORM_MANIFESTS = ('apps/web/Cargo.toml', 'apps/installer/Cargo.toml', 'fixtures/*/Cargo.toml')
PIN_DIR = Path('.local/composition')
PIN_FILES = ('Cargo.lock', 'manifest.txt')
# A release is a Git tag `vX.Y.Z` (docs/versions.md); the version is what follows the `v`.
VERSION_TAG = re.compile(r'v(\d+\.\d+\.\d+)')


class Refused(Exception):
    pass


def package_name(manifest):
    return tomllib.loads(manifest.read_text())['package']['name']


def snake_id(value, what, where):
    if not isinstance(value, str) or not SNAKE.fullmatch(value):
        raise Refused(f'{where}: {what} {value!r} must be snake_case ([a-z][a-z0-9_]*)')
    if value in RESERVED_IDS:
        raise Refused(f'{where}: {what} {value!r} is reserved ({", ".join(sorted(RESERVED_IDS))})')
    return value


def container_modules(root, layer):
    """Validate each mounted container of a Domain or Extension root; yield (path, crate)."""
    role = CONTAINER_LAYERS[layer]
    for folder in sorted((root / 'apps' / layer).iterdir()):
        if folder.name.startswith('.') or folder.name == 'src' or not (folder.is_dir() or folder.is_symlink()):
            continue
        where = f'apps/{layer}/{folder.name}'
        if folder.is_symlink():
            raise Refused(f'{where}: a container is mounted as a real checkout, not a symlink')
        for stray in ('Cargo.toml', 'Cargo.lock'):
            if (folder / stray).exists():
                raise Refused(f'{where}: a container has no root {stray}; only its modules are crates')
        descriptor = folder / CONTAINER_FILE
        if not descriptor.is_file():
            raise Refused(f'{where}: missing {CONTAINER_FILE}')
        try:
            declared = tomllib.loads(descriptor.read_text())
        except tomllib.TOMLDecodeError as error:
            raise Refused(f'{where}/{CONTAINER_FILE}: {error}') from None
        unknown = sorted(set(declared) - {'id', 'layer'})
        if unknown:
            raise Refused(f'{where}/{CONTAINER_FILE}: unknown keys {unknown}; expected id and layer')
        container = snake_id(declared.get('id'), 'container id', f'{where}/{CONTAINER_FILE}')
        if container != folder.name:
            raise Refused(f'{where}: {CONTAINER_FILE} declares id {container!r}; mount it at apps/{layer}/{container}')
        if declared.get('layer') != role:
            layer_folder = {v: k for k, v in CONTAINER_LAYERS.items()}.get(declared.get('layer'))
            hint = f'; mount it under apps/{layer_folder}' if layer_folder else ''
            raise Refused(f'{where}: {CONTAINER_FILE} declares layer {declared.get("layer")!r}, '
                          f'but apps/{layer} holds layer {role!r}{hint}')
        for manifest in sorted(folder.glob('*/Cargo.toml')):
            module = manifest.parent
            if module.name.startswith('.') or module.name == 'fixtures':
                continue
            if module.is_symlink():
                raise Refused(f'{where}/{module.name}: a module is a real folder, not a symlink')
            snake_id(module.name, 'module folder', f'{where}/{module.name}')
            name = package_name(manifest)
            expected = f'bilibi-{container}-{module.name}'.replace('_', '-')
            if name != expected:
                raise Refused(f'{where}/{module.name}/Cargo.toml: crate {name!r} must be named {expected!r}')
            yield f'{container}/{module.name}', name


def discovered(root):
    """Map each layer to its [(relative path, crate)] list."""
    result = {}
    names = set()
    containers = {}
    for layer in LAYERS:
        if layer in CONTAINER_LAYERS:
            modules = list(container_modules(root, layer))
        else:
            modules = [(manifest.parent.name, package_name(manifest))
                       for manifest in sorted((root / 'apps' / layer).glob('*/Cargo.toml'))]
        for path, name in modules:
            if name in names:
                raise Refused(f'Duplicate module crate: {name}')
            names.add(name)
            if layer in CONTAINER_LAYERS:
                container = path.split('/')[0]
                if containers.setdefault(container, layer) != layer:
                    raise Refused(f'Container {container!r} is mounted under both apps/domains and apps/extensions')
        result[layer] = modules
    return result


def composition(layer, modules):
    text = (f'# Generated by scripts/discover-modules.py; edit deep-module manifests.\n'
            f'[package]\nname = "bilibi-{layer}"\nversion.workspace = true\n'
            'edition.workspace = true\nrust-version.workspace = true\nlicense.workspace = true\n\n[dependencies]\n')
    text += ''.join(f'{name} = {{ path = "{path}" }}\n' for path, name in modules)
    code = '// Generated composition links. Runtime inventory owns discovery.\n'
    code += ''.join(f'pub use {name.replace("-", "_")} as {path.replace("/", "_")};\n' for path, name in modules)
    if layer in CONTAINER_LAYERS:
        # A mounted module's ID is its <container>/<module> path; boot refuses descriptors that differ.
        code += '/// Mounted (module ID, crate) pairs, checked against registered descriptors at boot.\n#[rustfmt::skip]\n'
        code += 'pub const MODULES: &[(&str, &str)] = &[' + ', '.join(f'("{path}", "{name}")' for path, name in modules) + '];\n'
    code += 'pub fn link() {}\n'
    return {f'apps/{layer}/Cargo.toml': text, f'apps/{layer}/src/lib.rs': code}


def outputs(root, modules):
    result = {}
    for layer in LAYERS:
        result.update({root / path: text for path, text in composition(layer, modules[layer]).items()})
    return result


def platform_crates(root, modules):
    names = {f'bilibi-{layer}' for layer in LAYERS}
    names.update(name for layer in ('base', 'core') for _, name in modules[layer])
    for pattern in PLATFORM_MANIFESTS:
        names.update(package_name(manifest) for manifest in root.glob(pattern))
    return names


def git_versions(root, path):
    """Yield (label, text) for the committed and staged forms of a tracked file."""
    for label, spec in (('committed', f'HEAD:{path}'), ('staged', f':{path}')):
        shown = subprocess.run(['git', '-C', str(root), 'show', spec], capture_output=True, text=True)
        if shown.returncode == 0:
            yield label, shown.stdout


def check_unmounted_commit(root, modules):
    """Refuse a staged or committed composition that names mounted Domain or Extension crates."""
    inside = subprocess.run(['git', '-C', str(root), 'rev-parse', '--show-toplevel'],
                            capture_output=True, text=True)
    if inside.returncode != 0 or Path(inside.stdout.strip()).resolve() != root.resolve():
        raise Refused('--check needs the Platform Git checkout to read its staged and committed composition')
    advice = ('Never commit composition files or Cargo.lock from a mounted checkout; '
              'unmount, run python3 scripts/discover-modules.py, and let Cargo prune Cargo.lock (see apps/domains/AGENTS.md)')
    for layer in CONTAINER_LAYERS:
        for path, text in composition(layer, []).items():
            for label, shown in git_versions(root, path):
                if shown != text:
                    raise Refused(f'The {label} {path} is a mounted composition. {advice}')
    allowed = platform_crates(root, modules)
    for label, shown in git_versions(root, 'Cargo.lock'):
        lock = tomllib.loads(shown)
        foreign = sorted(p['name'] for p in lock.get('package', []) if 'source' not in p and p['name'] not in allowed)
        if foreign:
            raise Refused(f'The {label} Cargo.lock names path packages outside the Platform: {", ".join(foreign)}. {advice}')


def git(path, *args):
    shown = subprocess.run(['git', '-C', str(path), *args], capture_output=True, text=True)
    if shown.returncode != 0:
        raise Refused(f'git {" ".join(args)} failed in {path}: {shown.stderr.strip() or "not a Git checkout"}')
    return shown.stdout


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def mounted_containers(root, modules):
    """Map `<layer>/<container>` to its folder for every container that mounts modules."""
    return {f'{layer}/{path.split("/")[0]}': root / 'apps' / layer / path.split('/')[0]
            for layer in CONTAINER_LAYERS for path, _ in modules[layer]}


def checkout_head(label, folder):
    """HEAD of a clean, independent checkout; a company build records nothing less."""
    if Path(git(folder, 'rev-parse', '--show-toplevel').strip()).resolve() != folder.resolve():
        raise Refused(f'{label} is not its own Git checkout; mount it with git clone')
    if git(folder, 'status', '--porcelain').strip():
        raise Refused(f'{label} has uncommitted or untracked files; commit them in its repository first')
    return git(folder, 'rev-parse', 'HEAD').strip()


def version_at_head(label, folder):
    """The version a `vX.Y.Z` tag at HEAD names, or None when HEAD carries no version tag."""
    tags = git(folder, 'tag', '--points-at', 'HEAD').split()
    versions = sorted({match.group(1) for tag in tags if (match := VERSION_TAG.fullmatch(tag))})
    if len(versions) > 1:
        raise Refused(f'{label} carries several version tags at HEAD ({", ".join("v" + v for v in versions)}); keep one')
    return versions[0] if versions else None


def workspace_version(root):
    return tomllib.loads((root / 'Cargo.toml').read_text())['workspace']['package']['version']


def require_resolved_lock(root):
    resolved = subprocess.run(['cargo', 'metadata', '--locked', '--format-version', '1'],
                              cwd=root, capture_output=True, text=True)
    if resolved.returncode != 0:
        raise Refused('Cargo.lock does not resolve the mounted composition; run any Cargo command '
                      f'(for example cargo metadata) without --locked first. {resolved.stderr.strip()}')


def pin(root, modules, files):
    """Record the Platform and container HEADs and the resolved Cargo.lock under .local/composition."""
    changed = [line[3:] for line in git(root, 'status', '--porcelain', '--untracked-files=no').splitlines()]
    composition_files = {str(path.relative_to(root)) for path in files} | {'Cargo.lock'}
    unexpected = sorted(set(changed) - composition_files)
    if unexpected:
        raise Refused(f'Platform has uncommitted changes outside the composition: {", ".join(unexpected)}')
    mounted = mounted_containers(root, modules)
    if not mounted:
        raise Refused('Nothing is mounted under apps/domains or apps/extensions; there is nothing to pin')
    heads = {name: checkout_head(f'apps/{name}', folder) for name, folder in sorted(mounted.items())}
    versions = {name: version_at_head(f'apps/{name}', folder) for name, folder in sorted(mounted.items())}
    require_resolved_lock(root)
    lock = root / 'Cargo.lock'
    platform = git(root, 'rev-parse', 'HEAD').strip()
    platform_version = workspace_version(root)
    tagged = version_at_head('The Platform', root)
    if tagged is not None and tagged != platform_version:
        raise Refused(f'The Platform is tagged v{tagged} but Cargo.toml says version {platform_version}; '
                      'a release tag names the Cargo.toml version (docs/versions.md)')
    manifest = ('# Written by scripts/discover-modules.py --pin; read by --restore and apps/web/build.rs.\n'
                f'platform = "{platform}"\n'
                f'platform_version = "{platform_version}"\n'
                + (f'platform_tag = "v{tagged}"\n' if tagged else '')
                + f'cargo_lock_sha256 = "{sha256(lock)}"\n')
    for name, head in heads.items():
        manifest += f'\n[containers."{name}"]\nhead = "{head}"\n'
        if versions[name]:
            manifest += f'version = "{versions[name]}"\n'
    target = root / PIN_DIR
    target.mkdir(parents=True, exist_ok=True)
    # Written only when changed: the executable host's build script watches
    # this folder, so an untouched pin keeps the compiled crates fresh.
    if not (target / 'Cargo.lock').is_file() or (target / 'Cargo.lock').read_bytes() != lock.read_bytes():
        shutil.copyfile(lock, target / 'Cargo.lock')
    if not (target / 'manifest.txt').is_file() or (target / 'manifest.txt').read_text() != manifest:
        (target / 'manifest.txt').write_text(manifest)
    described = ', '.join(f'{name} {"v" + versions[name] if versions[name] else head[:12] + " (untagged)"}'
                          for name, head in heads.items())
    platform_label = f'v{tagged}' if tagged else f'{platform[:12]} (unreleased {platform_version})'
    print(f'pinned {len(heads)} container(s) at Platform {platform_label} in {PIN_DIR}: {described}')


def restore(root, modules, files):
    """Rebuild the pinned composition: refuse any drift, then put the pinned Cargo.lock back."""
    target = root / PIN_DIR
    missing = [name for name in PIN_FILES if not (target / name).is_file()]
    if missing:
        raise Refused(f'{PIN_DIR} lacks {", ".join(missing)}; run --pin on the machine that built the release')
    try:
        manifest = tomllib.loads((target / 'manifest.txt').read_text())
        platform, digest, pinned = manifest['platform'], manifest['cargo_lock_sha256'], manifest['containers']
    except (tomllib.TOMLDecodeError, KeyError) as error:
        raise Refused(f'{PIN_DIR}/manifest.txt is unreadable ({error!r}); run --pin again') from None
    head = git(root, 'rev-parse', 'HEAD').strip()
    if head != platform:
        raise Refused(f'Platform is at {head[:12]} but the pin was made at {platform[:12]}; check out the pinned revision')
    if sha256(target / 'Cargo.lock') != digest:
        raise Refused(f'{PIN_DIR}/Cargo.lock does not match the pinned SHA-256; run --pin again')
    mounted = mounted_containers(root, modules)
    for name in sorted(set(pinned) - set(mounted)):
        raise Refused(f'apps/{name} is pinned but not mounted; clone it and check out {pinned[name]["head"][:12]}')
    for name in sorted(set(mounted) - set(pinned)):
        raise Refused(f'apps/{name} is mounted but not in the pin; unmount it or run --pin again')
    for name, folder in sorted(mounted.items()):
        actual = checkout_head(f'apps/{name}', folder)
        if actual != pinned[name]['head']:
            raise Refused(f'apps/{name} is at {actual[:12]} but the pin was made at {pinned[name]["head"][:12]}; '
                          f'run git -C apps/{name} checkout {pinned[name]["head"]}')
    write_outputs(files)
    shutil.copyfile(target / 'Cargo.lock', root / 'Cargo.lock')
    print(f'restored {len(pinned)} container(s) and Cargo.lock; build with cargo build --release --locked')


def write_outputs(files):
    for path, text in files.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        if not path.exists() or path.read_text() != text:
            path.write_text(text)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--check', action='store_true',
                      help='refuse stale composition files and a staged or committed mounted composition')
    mode.add_argument('--pin', action='store_true',
                      help='record the mounted container HEADs and the resolved Cargo.lock in .local/composition')
    mode.add_argument('--restore', action='store_true',
                      help='rebuild the pinned composition, refusing any container or Platform drift')
    args = parser.parse_args()
    try:
        modules = discovered(ROOT)
        files = outputs(ROOT, modules)
        if args.restore:
            restore(ROOT, modules, files)
        elif args.pin:
            write_outputs(files)
            pin(ROOT, modules, files)
        elif args.check:
            for path, text in files.items():
                if not path.exists() or path.read_text() != text:
                    raise Refused(f'Composition is stale: {path}. Run python3 scripts/discover-modules.py')
            check_unmounted_commit(ROOT, modules)
        else:
            write_outputs(files)
    except Refused as refusal:
        raise SystemExit(f'discover-modules: {refusal}') from None


if __name__ == '__main__':
    main()
