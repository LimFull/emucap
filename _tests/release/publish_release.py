"""Publish a draft only after its complete remote asset inventory matches prepared bytes."""
import argparse
from pathlib import Path
import re
import subprocess
from urllib.parse import quote

from native_payload import MAX_PAYLOAD_BYTES, bounded_read, digest_file, portable_path, regular
from native_producer import command, parse_json, positive_id, repository_name


def github(path):
    # Release state is mutable: never reuse the producer metadata cache here.
    return parse_json(command('gh', 'api', path))


def release_assets(repo, release_id):
    assets = []
    for page in range(1, 17):
        part = github(f'repos/{repo}/releases/{release_id}/assets?per_page=100&page={page}')
        if not isinstance(part, list):
            raise ValueError('invalid release asset response')
        assets.extend(part)
        if len(part) < 100:
            return assets
    raise ValueError('release asset inventory exceeds limit')


def check_assets(assets, expected, complete):
    seen, ids = set(), set()
    for asset in assets:
        name = asset['name']
        identity = positive_id(asset['id'])
        if name in seen or identity in ids or name not in expected:
            raise ValueError('unexpected or duplicate remote release asset')
        if (asset.get('state') != 'uploaded'
                or (asset.get('size'), asset.get('digest')) != expected[name]):
            raise ValueError('remote release asset differs from prepared bytes')
        seen.add(name)
        ids.add(identity)
    if complete and seen != set(expected):
        raise ValueError('remote release asset inventory is incomplete')
    return seen


def check_draft(repo, release, tag, revision):
    if release.get('draft') is not True or release.get('tag_name') != tag:
        raise ValueError('release must remain the selected draft')
    target = github(f'repos/{repo}/git/ref/tags/{quote(tag, safe="")}')['object']
    for _ in range(8):
        if target['type'] == 'commit' and target['sha'] == revision:
            return
        if target['type'] != 'tag' or not re.fullmatch(r'[0-9a-f]{40}', target['sha']):
            break
        target = github(f'repos/{repo}/git/tags/{target["sha"]}')['object']
    raise ValueError('release tag differs from selected source revision')


def publish(repo, tag, revision, asset_list):
    repository_name(repo)
    if not re.fullmatch(r'[0-9a-f]{40}', revision) or not tag or tag.startswith('-'):
        raise ValueError('invalid release identity')
    paths = parse_json(bounded_read(asset_list))
    if not isinstance(paths, list) or not 1 <= len(paths) <= 1024:
        raise ValueError('invalid prepared release asset list')
    selected, expected, total = {}, {}, 0
    for value in paths:
        path = Path(value)
        name = str(portable_path(path.name))
        if name in selected:
            raise ValueError('duplicate prepared release asset')
        size = regular(path).st_size
        total += size
        if total > MAX_PAYLOAD_BYTES:
            raise ValueError('prepared release assets exceed byte limit')
        expected[name] = (size, 'sha256:' + digest_file(path))
        selected[name] = str(path)
    release = github(f'repos/{repo}/releases/tags/{quote(tag, safe="")}')
    release_id = positive_id(release['id'])
    check_draft(repo, release, tag, revision)
    present = check_assets(release_assets(repo, release_id), expected, complete=False)
    missing = [selected[name] for name in sorted(set(selected) - present)]
    if missing:
        subprocess.run(['gh', 'release', 'upload', '--repo', repo, tag, *missing], check=True)
    # Read the full inventory again after upload. Existing matching files permit
    # retrying a partial upload; extra or changed draft files prevent publication.
    check_draft(repo, github(f'repos/{repo}/releases/{release_id}'), tag, revision)
    check_assets(release_assets(repo, release_id), expected, complete=True)
    prerelease = '-rc.' in tag
    subprocess.run(['gh', 'api', '--method', 'PATCH', f'repos/{repo}/releases/{release_id}',
                    '-F', 'draft=false', '-F', f'prerelease={str(prerelease).lower()}',
                    '-f', f'make_latest={str(not prerelease).lower()}'], check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('repo', 'tag', 'revision'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--asset-list', type=Path, required=True)
    args = parser.parse_args()
    publish(args.repo, args.tag, args.revision, args.asset_list)


if __name__ == '__main__':
    main()
