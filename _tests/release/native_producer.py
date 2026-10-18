"""Producer policy and native input identities shared by release artifact receivers."""
from functools import lru_cache
import hashlib
import json
import json
import os
from pathlib import Path
import re
import subprocess


MSVC = {'mesen2', 'dolphin', 'ppsspp'}
MSYS2 = {'mednafen', 'flycast', 'np2kai', 'mupen64plus', 'openmsx',
         'mame-pc98', 'mame-neogeo', 'pcsx2', 'desmume-nds'}
ADAPTERS = MSVC | MSYS2 | {'xemu'}
TRUSTED_REFS = {'refs/heads/main', 'refs/heads/next'}
SHA = re.compile(r'[0-9a-f]{40}')
MAX_METADATA_BYTES = 16 * 1024 * 1024

# Reviewed complete pipeline input maps for the 1.1.0 native builds. Approval is
# scoped to an exact repository, revision and map; all remaining admission checks
# still apply. Never infer approval from ancestry or a successful workflow alone.
APPROVED_PIPELINES = {
    ('mcpads/emucap', 'ebc59d84f62ad452fd8e9fca7d15af3926f0bed9'):
        'd4c8f1b23add068eddfa739be3835bf9200f2d5e45633987aa30dd95d991303b',
    ('mcpads/emucap', 'b47ab0b18462d6a715f78c763a64a43db1fc5138'):
        '6434a8c5c0bd5c4a2c063dbceb90c1f49bbf6193c1c1c5c403a728003b90f7f1',
}


def pipeline_digest(inputs):
    return hashlib.sha256(json.dumps(inputs, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def command(*args):
    return subprocess.check_output([str(a) for a in args], text=True).strip()


def unique_object(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError(f'duplicate JSON key: {key}')
        value[key] = item
    return value


def parse_json(raw):
    if len(raw) > MAX_METADATA_BYTES:
        raise ValueError('native metadata exceeds byte limit')
    return json.loads(raw, object_pairs_hook=unique_object,
                      parse_constant=lambda _: (_ for _ in ()).throw(ValueError('non-finite JSON')))


@lru_cache(maxsize=512)
def github(path):
    return parse_json(command('gh', 'api', path))


def github_pages(path, key):
    result = []
    for page in range(1, 101):
        value = github(f'{path}?per_page=100&page={page}')[key]
        result.extend(value)
        if len(value) < 100:
            return result
    raise ValueError('native producer metadata exceeds page limit')


def positive_id(value):
    if isinstance(value, bool) or not re.fullmatch(r'[1-9][0-9]{0,19}', str(value)):
        raise ValueError('invalid native producer ID')
    return int(value)


def repository_name(value):
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', value):
        raise ValueError('invalid native repository')
    return value


def producer_spec(purpose, adapter=None, platform=None):
    if purpose == 'assembly':
        if adapter is not None or platform is not None:
            raise ValueError('assembly has no adapter or platform')
        return 'native-unix.yml', 'assemble', 'native-release-packages'
    if adapter not in ADAPTERS or platform not in ('linux', 'macos', 'windows'):
        raise ValueError('unknown native producer matrix entry')
    if purpose == 'package':
        job = 'package' if platform == 'windows' else f'build ({adapter}, {platform})'
        return 'native-unix.yml', job, f'native-package-{adapter}-{platform}'
    if purpose == 'runtime' and platform == 'windows':
        workflow = ('native-adapters.yml' if adapter in MSVC else
                    'native-msys2.yml' if adapter in MSYS2 else 'native-xemu-windows.yml')
        job = 'build' if adapter == 'xemu' else f'windows ({adapter})'
        return workflow, job, f'native-runtime-{adapter}-windows-x86_64'
    raise ValueError('unknown native producer purpose')


def tracked(root, *paths):
    raw = subprocess.check_output(['git', '-C', str(root), 'ls-files', '-z', '--', *paths])
    return sorted(set(filter(None, raw.decode().split('\0'))))


def input_hashes(root, names):
    result = {}
    for name in names:
        path = root / name
        if path.is_symlink() or not path.is_file():
            raise ValueError(f'native input is not a regular file: {name}')
        result[name] = hashlib.sha256(path.read_bytes()).hexdigest()
    return result


def workflow_path(root, name):
    overlay = f'tools/deploy/public/.github/workflows/{name}'
    return overlay if (root / overlay).is_file() else f'.github/workflows/{name}'


def native_inputs(source, pipeline, adapter, platform):
    producer_spec('package', adapter, platform)
    paths = [f'adapters/{adapter}']
    if adapter == 'mame-neogeo':
        paths.append('adapters/mame-pc98')
    if platform != 'windows' or adapter not in MSVC:
        paths.append('adapters/_common')
        paths.append('_tests/adapters/build-lock-test.sh')
    if platform != 'windows' and adapter == 'np2kai':
        paths.append('_tests/native/np2kai-load-api.py')
        paths.append('src/np2kai_adapter/ffi.rs')
    names = [name for name in tracked(source, *paths) if not name.endswith('.md')]
    owner = 'mame-pc98' if adapter == 'mame-neogeo' else adapter
    if f'adapters/{owner}/upstream.lock' not in names:
        raise ValueError('native inputs require a tracked upstream lock')
    source_files = input_hashes(source, names)
    # Packaging policy changes are separate from the compiler recipe. MSVC's
    # established cache owner already incorporates its build workflow/toolchain.
    if platform != 'windows':
        builder = input_hashes(pipeline, ['_tests/release/native_unix_build.sh'])
    else:
        name, _, _ = producer_spec('runtime', adapter, platform)
        path = workflow_path(source, name)
        builder = {f'.github/workflows/{name}': input_hashes(source, [path])[path]}
        if adapter in MSVC:
            builder.update(input_hashes(source, ['_tests/release/native_build.py']))
        elif adapter in MSYS2:
            builder.update(input_hashes(source, ['_tests/release/windows_native_payload.py',
                                                '_tests/release/native_build.py']))
    return {'source': source_files, 'builder': builder}


def pipeline_paths(root):
    # This is the complete release-helper directory, including future imports.
    # Tests do not execute in a producer after compilation and remain included so
    # changes to an admission check cannot silently reuse an older pipeline.
    paths = [name for name in tracked(root, '_tests/release') if not name.endswith('.md')]
    names = ('native-unix.yml', 'native-adapters.yml', 'native-msys2.yml',
             'native-xemu-windows.yml')
    mapping = {name: name for name in paths}
    mapping['_tests/adapters/build-lock-test.sh'] = '_tests/adapters/build-lock-test.sh'
    mapping['_tests/native/np2kai-load-api.py'] = '_tests/native/np2kai-load-api.py'
    for name in names:
        mapping[f'.github/workflows/{name}'] = workflow_path(root, name)
    for name in ('native_producer.py', 'native_provenance.py', 'native_payload.py'):
        if f'_tests/release/{name}' not in mapping:
            raise ValueError(f'missing tracked provenance helper: {name}')
    return mapping


def pipeline_blobs(root):
    result = {}
    for public, local in pipeline_paths(root).items():
        path = root / local
        if path.is_symlink() or not path.is_file():
            raise ValueError(f'unsafe pipeline input: {local}')
        raw = path.read_bytes()
        result[public] = hashlib.sha1(b'blob ' + str(len(raw)).encode() + b'\0' + raw).hexdigest()
    return result


def admit_pipeline(info, authority, repo):
    tree = github(f'repos/{repo}/git/trees/{info["head_sha"]}?recursive=1')
    if tree.get('truncated'):
        raise ValueError('native pipeline tree is truncated')
    entries = {item['path']: item for item in tree['tree']}
    expected = pipeline_blobs(authority)
    actual = {name: item['sha'] for name, item in entries.items()
              if name.startswith('_tests/release/') and not name.endswith('.md')
              and item['type'] != 'tree'}
    for name in expected:
        if not name.startswith('_tests/release/'):
            actual[name] = entries.get(name, {}).get('sha')
    reviewed = APPROVED_PIPELINES.get((repo, info['head_sha'])) == pipeline_digest(actual)
    if (actual != expected and not reviewed) or any(entries[name].get('mode') not in ('100644', '100755')
                                 for name in actual if name in entries):
        raise ValueError('native producer pipeline inputs are not approved')


def admit_run(info, jobs, repository, workflow, purpose, adapter=None, platform=None):
    filename, job_name, _ = producer_spec(purpose, adapter, platform)
    repo = repository['full_name']
    path = f'.github/workflows/{filename}'
    events = {'workflow_dispatch'} if filename == 'native-unix.yml' else {'push', 'workflow_dispatch'}
    if (info.get('status') not in ('queued', 'in_progress', 'completed') or info.get('event') not in events
            or info.get('repository', {}).get('id') != repository['id']
            or info.get('repository', {}).get('full_name') != repo
            or info.get('head_repository', {}).get('id') != repository['id']
            or info.get('head_repository', {}).get('full_name') != repo
            or f'refs/heads/{info.get("head_branch")}' not in TRUSTED_REFS
            or not SHA.fullmatch(info.get('head_sha', ''))
            or info.get('workflow_id') != workflow.get('id')
            or workflow.get('path') != path
            or info.get('path', '').split('@')[0] != path):
        raise ValueError('native run is not an approved producer')
    run_id, attempt = positive_id(info['id']), positive_id(info['run_attempt'])
    candidates = [job for job in jobs if job.get('name') == job_name]
    if (len(candidates) != 1 or candidates[0].get('status') != 'completed'
            or candidates[0].get('conclusion') != 'success'
            or candidates[0].get('run_id') != run_id
            or candidates[0].get('run_attempt') != attempt
            or candidates[0].get('head_sha') != info['head_sha']):
        raise ValueError('exact native producer job did not succeed')
    return {'repository': repo, 'repository_id': repository['id'], 'workflow': path,
            'revision': info['head_sha'], 'ref': f'refs/heads/{info["head_branch"]}',
            'run': run_id, 'attempt': attempt, 'event': info['event'], 'job': job_name}


def resolve_producer(repo, run_id, attempt, purpose, adapter, platform, authority):
    repository_name(repo)
    run_id = positive_id(run_id)
    info = github(f'repos/{repo}/actions/runs/{run_id}')
    attempt = positive_id(attempt if attempt is not None else info['run_attempt'])
    if attempt != info['run_attempt']:
        info = github(f'repos/{repo}/actions/runs/{run_id}/attempts/{attempt}')
    if info.get('id') != run_id or info.get('run_attempt') != attempt:
        raise ValueError('native run response does not match the selected invocation')
    filename, _, _ = producer_spec(purpose, adapter, platform)
    jobs = github_pages(f'repos/{repo}/actions/runs/{run_id}/attempts/{attempt}/jobs', 'jobs')
    identity = admit_run(info, jobs, github(f'repos/{repo}'),
                         github(f'repos/{repo}/actions/workflows/{filename}'),
                         purpose, adapter, platform)
    admit_pipeline(info, authority, repo)
    return identity


def current_producer(pipeline, purpose, adapter, platform):
    filename, job, _ = producer_spec(purpose, adapter, platform)
    repo = repository_name(os.environ['GITHUB_REPOSITORY'])
    revision = command('git', '-C', pipeline, 'rev-parse', 'HEAD')
    ref, event = os.environ['GITHUB_REF'], os.environ['GITHUB_EVENT_NAME']
    events = {'workflow_dispatch'} if filename == 'native-unix.yml' else {'push', 'workflow_dispatch'}
    if (ref not in TRUSTED_REFS or event not in events or revision != os.environ['GITHUB_SHA']
            or os.environ['GITHUB_WORKFLOW_REF'] != f'{repo}/.github/workflows/{filename}@{ref}'):
        raise ValueError('current workflow is not an approved native producer')
    return {'repository': repo, 'repository_id': positive_id(os.environ['GITHUB_REPOSITORY_ID']),
            'workflow': f'.github/workflows/{filename}', 'revision': revision, 'ref': ref,
            'run': positive_id(os.environ['GITHUB_RUN_ID']),
            'attempt': positive_id(os.environ['GITHUB_RUN_ATTEMPT']), 'event': event, 'job': job}


def admit_source(source, pipeline):
    revision = command('git', '-C', source, 'rev-parse', 'HEAD')
    pipeline_revision = command('git', '-C', pipeline, 'rev-parse', 'HEAD')
    if not SHA.fullmatch(revision) or not SHA.fullmatch(pipeline_revision):
        raise ValueError('native source must resolve to an exact commit')
    # Both checkouts come from the workflow repository with full history.
    subprocess.run(['git', '-C', str(pipeline), 'merge-base', '--is-ancestor',
                    revision, pipeline_revision], check=True)
    if command('git', '-C', source, 'status', '--porcelain', '--untracked-files=no'):
        raise ValueError('native source has tracked changes')
    return revision
