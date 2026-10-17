"""Prepare an exact release asset set from authenticated native assembly and core outputs."""
import argparse
import json
from pathlib import Path
import re
import shutil
import tarfile
import tomllib

from collect_native import ADAPTERS, PREBUILT_PLATFORMS
from native_payload import PARENTS, RECORD, SIGNATURE, bounded_read, digest_file, payload_files
from native_producer import parse_json
from native_provenance import fetch


def checksum_manifest(raw):
    result = {}
    for line in raw.decode('ascii').splitlines():
        match = re.fullmatch(r'([0-9a-f]{64})  ([A-Za-z0-9_.-]+)', line)
        if not match or match[2] in result:
            raise ValueError('invalid release checksum manifest')
        result[match[2]] = match[1]
    return result


def native_assets(verified):
    directory, record = verified.directory, verified.record
    index = parse_json(bounded_read(directory / 'NATIVE-PACKAGES.json'))
    if set(index) != {'core_revision', 'packages'} or index['core_revision'] != record['release_revision']:
        raise ValueError('native assembly index revision mismatch')
    expected = {(a, p) for a in ADAPTERS for p in PREBUILT_PLATFORMS[a]}
    selected = [(p['adapter'], p['platform']) for p in index['packages']]
    if set(selected) != expected or len(selected) != len(expected):
        raise ValueError('native assembly index matrix mismatch')
    parents = {digest: verified.proofs[digest][0] for digest in record['parents']}
    names = {'NATIVE-PACKAGES.json', 'NATIVE-SHA256SUMS'}
    seen = set()
    for package in index['packages']:
        digest = package['provenance_sha256']
        if digest not in parents or digest in seen:
            raise ValueError('native index does not bind its package parents')
        seen.add(digest)
        parent = parents[digest]
        if (parent['purpose'] != 'package'
                or (parent['adapter'], parent['platform']) != (package['adapter'], package['platform'])
                or parent['producer']['run'] != package['workflow_run']
                or parent['producer']['revision'] != package['packaging_revision']
                or parent['native']['source_revision'] != package['source_revision']):
            raise ValueError('native index package producer mismatch')
        name = f'emucap-{package["version"]}-{package["adapter"]}-{package["target"]}.json'
        original = {k: v for k, v in package.items()
                    if k not in ('platform', 'workflow_run', 'provenance_sha256')}
        if parse_json(bounded_read(directory / name)) != original:
            raise ValueError('native index package record mismatch')
        for artifact, digest in package['artifacts'].items():
            if (artifact not in parent['files'] or parent['files'][artifact]['sha256'] != digest
                    or record['files'].get(artifact) != parent['files'][artifact]):
                raise ValueError('native index artifact differs from authenticated parent')
        if record['files'].get(name) != parent['files'].get(name):
            raise ValueError('native package metadata differs from authenticated parent')
        names.update(package['artifacts'])
        names.add(name)
    if seen != set(parents):
        raise ValueError('native assembly has unselected package parents')
    actual = {name for name in record['files'] if not name.startswith(PARENTS + '/')}
    if actual != names:
        raise ValueError('native assembly has undeclared release assets')
    checksums = checksum_manifest(bounded_read(directory / 'NATIVE-SHA256SUMS'))
    if checksums != {name: record['files'][name]['sha256'] for name in names - {'NATIVE-SHA256SUMS'}}:
        raise ValueError('native release checksum set mismatch')
    return names


def core_assets(directory, version, revision):
    targets = {'x86_64-unknown-linux-gnu': '.tar.gz', 'aarch64-apple-darwin': '.tar.gz',
               'x86_64-pc-windows-msvc': '.zip'}
    files = payload_files(directory)
    expected, archives = set(), set()
    for target, extension in targets.items():
        name = f'emucap-{version}-{target}'
        archive, checksum, metadata = name + extension, name + '.sha256', name + '.json'
        expected.update((archive, checksum, metadata))
        archives.update((archive, metadata))
        manifest = parse_json(bounded_read(directory / metadata))
        if (manifest['version'], manifest['source_revision'], manifest['target']) != (version, revision, target):
            raise ValueError('core release metadata identity mismatch')
        if checksum_manifest(bounded_read(directory / checksum)) != {archive: digest_file(directory / archive)}:
            raise ValueError('core release checksum mismatch')
    if set(files) != expected:
        raise ValueError('unexpected core release asset set')
    return archives


def prepare(verified, core, output, version):
    native_names = native_assets(verified)
    core_names = core_assets(core, version, verified.record['release_revision'])
    if native_names & core_names or output.exists():
        raise ValueError('release assets collide or destination exists')
    output.mkdir(parents=True)
    for root, names in ((verified.directory, native_names), (core, core_names)):
        for name in sorted(names):
            shutil.copy2(root / name, output / name)
            if root == verified.directory and digest_file(output / name) != verified.record['files'][name]['sha256']:
                raise ValueError('native asset changed during release preparation')
    # Preserve signed lineage without uploading one GitHub release asset per proof.
    proof_names = [RECORD, SIGNATURE, *sorted(n for n in verified.record['files'] if n.startswith(PARENTS + '/'))]
    with tarfile.open(output / 'NATIVE-PROVENANCE.tar.gz', 'w:gz') as archive:
        for name in proof_names:
            path = verified.directory / name
            info = archive.gettarinfo(path, arcname=name)
            info.uid = info.gid = info.mtime = 0
            info.uname = info.gname = ''
            with path.open('rb') as stream:
                archive.addfile(info, stream)
    names = sorted([*native_names, *core_names, 'NATIVE-PROVENANCE.tar.gz'])
    (output / 'SHA256SUMS').write_text(''.join(f'{digest_file(output / name)}  {name}\n' for name in names))
    return [*names, 'SHA256SUMS']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('source', 'core', 'native', 'output', 'asset-list'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--repo', required=True)
    parser.add_argument('--run', required=True)
    args = parser.parse_args()
    verified = fetch(args.repo, args.run, 'assembly', None, None, args.native, args.source)
    version = tomllib.loads((args.source / 'Cargo.toml').read_text())['package']['version']
    assets = prepare(verified, args.core, args.output, version)
    args.asset_list.write_text(json.dumps([str(args.output / name) for name in assets]) + '\n')


if __name__ == '__main__':
    main()
