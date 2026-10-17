"""Bounded, exact native payload trees and immutable Actions artifact downloads."""
import hashlib
from pathlib import Path, PurePosixPath
import re
import stat
import tempfile
import urllib.request
import zipfile

from inspect_github_artifact import artifact_download_url
from native_producer import github_pages, positive_id, producer_spec

RECORD = 'NATIVE-PROVENANCE.json'
SIGNATURE = 'NATIVE-PROVENANCE.sigstore.json'
PARENTS = 'provenance'
MAX_FILES = 65536
MAX_PAYLOAD_BYTES = 64 * 1024**3
MAX_ARCHIVE_BYTES = 32 * 1024**3
MAX_RECORD_BYTES = 8 * 1024**2


def portable_path(name):
    if not isinstance(name, str) or not name or len(name) > 1024:
        raise ValueError('invalid native member path')
    parts = name.split('/')
    reserved = {'CON', 'PRN', 'AUX', 'NUL'} | {f'{p}{n}' for p in ('COM', 'LPT') for n in range(1, 10)}
    if (any(p in ('', '.', '..') or p.endswith((' ', '.'))
            or p.split('.')[0].upper() in reserved for p in parts)
            or any(ord(c) < 32 or c in '\\:*?"<>|' for c in name)):
        raise ValueError('unsafe native member path')
    return PurePosixPath(name)


def regular(path):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or getattr(path, 'is_junction', lambda: False)():
        raise ValueError(f'native member is not a regular file: {path.name}')
    return info


def bounded_read(path, limit=MAX_RECORD_BYTES):
    if regular(path).st_size > limit:
        raise ValueError('native record exceeds byte limit')
    with path.open('rb') as stream:
        value = stream.read(limit + 1)
    if len(value) > limit:
        raise ValueError('native record exceeds byte limit')
    return value


def digest_file(path):
    regular(path)
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def payload_files(root):
    if root.is_symlink() or not root.is_dir() or getattr(root, 'is_junction', lambda: False)():
        raise ValueError('native payload must be a real directory')
    files, aliases, directories, total = {}, set(), set(), 0
    pending = [root]
    entries = 0
    while pending:
        directory = pending.pop()
        for path in directory.iterdir():
            entries += 1
            if entries > MAX_FILES:
                raise ValueError('native payload exceeds entry limit')
            name = path.relative_to(root).as_posix()
            portable_path(name)
            alias = name.casefold()
            if alias in aliases:
                raise ValueError('native payload has colliding names')
            aliases.add(alias)
            info = path.lstat()
            if stat.S_ISLNK(info.st_mode) or getattr(path, 'is_junction', lambda: False)():
                raise ValueError('native payload has a link')
            if stat.S_ISDIR(info.st_mode):
                directories.add(name)
                pending.append(path)
                continue
            regular(path)
            total += info.st_size
            if total > MAX_PAYLOAD_BYTES:
                raise ValueError('native payload exceeds byte limit')
            if name not in (RECORD, SIGNATURE):
                files[name] = {'bytes': info.st_size, 'sha256': digest_file(path)}
    expected_directories = {parent.as_posix() for name in files
                            for parent in PurePosixPath(name).parents if parent != PurePosixPath('.')}
    if directories != expected_directories:
        raise ValueError('native payload has an undeclared empty directory')
    return files


def validate_file_map(files):
    if not isinstance(files, dict) or not files or len(files) > MAX_FILES:
        raise ValueError('invalid native file manifest')
    aliases, total = set(), 0
    for name, entry in files.items():
        portable_path(name)
        if name in (RECORD, SIGNATURE) or name.casefold() in aliases:
            raise ValueError('invalid native file manifest path')
        aliases.add(name.casefold())
        if (not isinstance(entry, dict) or set(entry) != {'bytes', 'sha256'}
                or type(entry['bytes']) is not int or entry['bytes'] < 0
                or not re.fullmatch('[0-9a-f]{64}', entry['sha256'])):
            raise ValueError('invalid native file manifest entry')
        total += entry['bytes']
    if total > MAX_PAYLOAD_BYTES:
        raise ValueError('native file manifest exceeds byte limit')


def select_artifact(items, producer, purpose, adapter, platform):
    _, _, name = producer_spec(purpose, adapter, platform)
    candidates = [item for item in items if item.get('name') == name and not item.get('expired')]
    if len(candidates) != 1:
        raise ValueError('expected exactly one native artifact')
    artifact = candidates[0]
    origin = artifact.get('workflow_run', {})
    if (origin.get('id') != producer['run'] or origin.get('head_sha') != producer['revision']
            or origin.get('repository_id') != producer['repository_id']
            or origin.get('head_repository_id') != producer['repository_id']
            or origin.get('head_branch') != producer['ref'].removeprefix('refs/heads/')
            or type(artifact.get('size_in_bytes')) is not int
            or not 0 < artifact['size_in_bytes'] <= MAX_ARCHIVE_BYTES
            or not re.fullmatch('sha256:[0-9a-f]{64}', artifact.get('digest', ''))):
        raise ValueError('native artifact identity or digest is invalid')
    positive_id(artifact['id'])
    return artifact


def extract_download(archive_path, output):
    if output.exists():
        raise ValueError('native download destination must be absent')
    with zipfile.ZipFile(archive_path) as archive:
        members = archive.infolist()
        if not members or len(members) > MAX_FILES:
            raise ValueError('native download exceeds entry limit')
        aliases, total = set(), 0
        for member in members:
            name = member.filename.rstrip('/') if member.is_dir() else member.filename
            portable_path(name)
            mode = member.external_attr >> 16
            kind = stat.S_IFMT(mode)
            if (name.casefold() in aliases or member.flag_bits & 1
                    or kind not in (0, stat.S_IFDIR if member.is_dir() else stat.S_IFREG)):
                raise ValueError('unsafe native download member')
            aliases.add(name.casefold())
            total += member.file_size
            if total > MAX_PAYLOAD_BYTES:
                raise ValueError('native download exceeds byte limit')
        output.mkdir(parents=True)
        for member in members:
            path = output / member.filename
            if member.is_dir():
                path.mkdir(parents=True, exist_ok=True)
                continue
            path.parent.mkdir(parents=True, exist_ok=True)
            remaining = member.file_size
            with archive.open(member) as src, path.open('xb') as dst:
                while remaining:
                    data = src.read(min(1024 * 1024, remaining))
                    if not data:
                        raise ValueError('truncated native download')
                    dst.write(data)
                    remaining -= len(data)
                if src.read(1):
                    raise ValueError('native download member grew')


def download_payload(repo, producer, purpose, adapter, platform, output):
    items = github_pages(f'repos/{repo}/actions/runs/{producer["run"]}/artifacts', 'artifacts')
    artifact = select_artifact(items, producer, purpose, adapter, platform)
    # The redirect helper keeps the credential and signed URL out of command logs.
    url = artifact_download_url(repo, artifact['id'])
    with tempfile.TemporaryDirectory(prefix='native-download-') as temporary:
        path = Path(temporary) / 'artifact.zip'
        digest, total = hashlib.sha256(), 0
        with urllib.request.urlopen(url, timeout=60) as response, path.open('xb') as stream:
            while data := response.read(1024 * 1024):
                total += len(data)
                if total > MAX_ARCHIVE_BYTES:
                    raise ValueError('native archive exceeds byte limit')
                digest.update(data)
                stream.write(data)
        if f'sha256:{digest.hexdigest()}' != artifact['digest']:
            raise ValueError('downloaded native artifact digest mismatch')
        extract_download(path, output)
    return artifact
