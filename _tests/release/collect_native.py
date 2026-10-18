"""Assemble all platform packages and reject stale or conflicting release inputs."""
import argparse
import json
from pathlib import Path
import shutil
import tarfile
import tempfile
import tomllib
import zipfile

from package_native import run, sha
from native_producer import ADAPTERS, PLATFORMS, NATIVE_TARGETS, MSVC
from native_provenance import create_record, fetch, retain_parents

PREBUILT_PLATFORMS = {a: PLATFORMS for a in ADAPTERS}


def validate_selection(selection):
    expected = NATIVE_TARGETS
    actual = [(r['adapter'], r['platform']) for r in selection]
    if set(actual) != expected or len(actual) != len(expected):
        raise ValueError('release requires one package for every adapter and selected platform')
    for record in selection:
        if not str(record['run']).isdecimal(): raise ValueError('invalid run ID')


def validate_package(directory, source, adapter, platform):
    records = [json.loads(p.read_text()) for p in directory.glob('emucap-*.json')]
    if len(records) != 1: raise ValueError('expected one native package record')
    record = records[0]
    target = {'linux': 'x86_64-unknown-linux-gnu', 'macos': 'aarch64-apple-darwin',
              'windows': 'windows-x86_64'}[platform]
    if platform == 'macos' and adapter == 'pcsx2': target = 'x86_64-apple-darwin'
    version = tomllib.loads((source / 'Cargo.toml').read_text())['package']['version']
    if (record['adapter'], record['target'], record['version']) != (adapter, target, version):
        raise ValueError('native package identity mismatch')
    for name, digest in record['artifacts'].items():
        if Path(name).name != name or sha(directory / name) != digest:
            raise ValueError(f'native artifact digest mismatch: {name}')
    runtime_name = f'emucap-{version}-{adapter}-{target}'
    runtime = directory / (runtime_name + ('.zip' if platform == 'windows' else '.tar.gz'))
    if runtime.name not in record['artifacts']: raise ValueError('missing runtime archive')
    if platform == 'windows':
        with zipfile.ZipFile(runtime) as archive:
            manifest = json.loads(archive.read(runtime_name + '/NATIVE-PACKAGE.json'))
    else:
        with tarfile.open(runtime) as archive:
            manifest = json.load(archive.extractfile(runtime_name + '/NATIVE-PACKAGE.json'))
    for key in ('adapter', 'target', 'version', 'source_revision', 'executable'):
        if manifest[key] != record[key]: raise ValueError('runtime manifest identity mismatch')
    if manifest['executable'] not in manifest['files']:
        raise ValueError('runtime executable absent from file manifest')
    sources = [name for name in record['artifacts'] if name.endswith('-source.tar.gz')]
    if len(sources) != 1: raise ValueError('missing corresponding native source archive')
    owner = 'mame-pc98' if adapter == 'mame-neogeo' else adapter
    paths = [f'adapters/{owner}', f'adapters/{adapter}']
    if platform != 'windows' or adapter not in MSVC: paths.append('adapters/_common')
    tracked = run('git', '-C', source, 'ls-files', *paths).splitlines()
    required = [p for p in tracked if not p.endswith('.md')]
    recipe_archive = runtime_name + '-recipes.tar.gz' if platform == 'windows' else sources[0]
    expected_artifacts = {runtime.name, sources[0]}
    if platform == 'windows': expected_artifacts.add(recipe_archive)
    if set(record['artifacts']) != expected_artifacts:
        raise ValueError('unexpected native package artifact set')
    with tarfile.open(directory / recipe_archive) as archive:
        for rel in required:
            path = rel if platform == 'windows' else sources[0][:-7] + '/recipes/' + rel
            stream = archive.extractfile(path)
            if stream is None or stream.read() != (source / rel).read_bytes():
                raise ValueError(f'package incompatible with release source: {rel}')
    return record


def merge_file(path, output):
    target = output / path.name
    if target.exists():
        if sha(target) != sha(path): raise ValueError(f'conflicting release artifact: {path.name}')
    else:
        shutil.move(path, target)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--selection', type=Path, required=True)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--repo', required=True)
    args = parser.parse_args()
    selection = json.loads(args.selection.read_text())
    validate_selection(selection)
    args.output.mkdir(parents=True, exist_ok=True)
    if any(args.output.iterdir()): raise ValueError('release output must be empty')
    records, parents = [], []
    for item in selection:
        adapter, platform, producer = item['adapter'], item['platform'], str(item['run'])
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary) / 'package'
            verified = fetch(args.repo, producer, 'package', adapter, platform, directory,
                             args.source, attempt=item.get('attempt'))
            record = validate_package(directory, args.source, adapter, platform)
            if (record['source_revision'] != verified.record['native']['source_revision']
                    or record['packaging_revision'] != verified.record['producer']['revision']):
                raise ValueError('native package record differs from authenticated producer')
            records.append(dict(record, platform=platform, workflow_run=int(producer),
                                provenance_sha256=verified.digest))
            for name in [*record['artifacts'], f"emucap-{record['version']}-{adapter}-{record['target']}.json"]:
                merge_file(directory / name, args.output)
            parents.extend(retain_parents(args.output, [verified]))
        print(f'verified {adapter}/{platform}', flush=True)
    index = dict(core_revision=run('git', '-C', args.source, 'rev-parse', 'HEAD'), packages=records)
    (args.output / 'NATIVE-PACKAGES.json').write_text(json.dumps(index, indent=2) + '\n')
    checksums = ''.join(f'{sha(p)}  {p.name}\n' for p in sorted(args.output.iterdir()) if p.is_file())
    (args.output / 'NATIVE-SHA256SUMS').write_text(checksums)
    create_record(args.output, args.source, 'assembly', parents=parents)


if __name__ == '__main__': main()
