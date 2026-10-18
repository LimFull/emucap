"""Create and verify signed native producer records, payloads, and retained lineage."""
import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess

from native_payload import (PARENTS, RECORD, SIGNATURE, MAX_RECORD_BYTES, bounded_read,
                            digest_file, download_payload, payload_files, validate_file_map)
from native_producer import (NATIVE_TARGETS, SHA, admit_source, command, current_producer,
                             native_inputs, parse_json, pipeline_blobs, producer_spec, resolve_producer)

ROOT = Path(__file__).resolve().parents[2]
MAX_PARENTS = 256
MAX_DEPTH = 16
MAX_PROOF_BYTES = 32 * 1024**2


def canonical(value):
    return (json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=True) + '\n').encode()


def parent_paths(store, digest):
    if not isinstance(digest, str) or not re.fullmatch('[0-9a-f]{64}', digest):
        raise ValueError('invalid native parent digest')
    return store / (digest + '.json'), store / (digest + '.sigstore.json')


def check_signature_result(results, producer):
    invocation = (f'https://github.com/{producer["repository"]}/actions/runs/'
                  f'{producer["run"]}/attempts/{producer["attempt"]}')
    expected = {'runInvocationURI': invocation,
                'sourceRepositoryIdentifier': str(producer['repository_id']),
                'buildTrigger': producer['event']}
    if not isinstance(results, list) or not any(
            all(item.get('verificationResult', {}).get('signature', {}).get('certificate', {}).get(key) == value
                for key, value in expected.items()) for item in results):
        raise ValueError('native signing certificate does not match the producer invocation')


def verify_signature(record, signature, producer):
    # gh verifies signatures, trust roots, timestamps, subject digest and the
    # certificate identity. Run/attempt checks use certificate extensions, never
    # the workflow-authored SLSA predicate as independent identity evidence.
    result = command('gh', 'attestation', 'verify', record, '--bundle', signature,
                     '--repo', producer['repository'],
                     '--signer-workflow', producer['repository'] + '/' + producer['workflow'],
                     '--signer-digest', producer['revision'], '--source-digest', producer['revision'],
                     '--source-ref', producer['ref'], '--deny-self-hosted-runners', '--format', 'json')
    check_signature_result(parse_json(result), producer)


@dataclass
class VerifiedPayload:
    directory: Path
    record: dict
    digest: str
    proofs: dict


class Verifier:
    def __init__(self, repository, authority=ROOT):
        self.repository = repository
        self.authority = authority
        self.proofs = {}
        self.active = set()
        self.proof_bytes = 0

    def authenticate(self, path, signature, store, depth=0):
        raw = bounded_read(path)
        digest = hashlib.sha256(raw).hexdigest()
        if digest in self.active or depth > MAX_DEPTH:
            raise ValueError('native provenance lineage cycle or depth limit')
        if digest in self.proofs:
            return self.proofs[digest][0]
        self.proof_bytes += len(raw) + len(bounded_read(signature))
        if len(self.proofs) + len(self.active) >= MAX_PARENTS or self.proof_bytes > MAX_PROOF_BYTES:
            raise ValueError('native provenance lineage exceeds budget')
        record = parse_json(raw)
        if set(record) != {'purpose', 'adapter', 'platform', 'producer', 'native', 'files', 'parents', 'release_revision'}:
            raise ValueError('invalid native provenance record shape')
        purpose, adapter, platform = (record[key] for key in ('purpose', 'adapter', 'platform'))
        producer_spec(purpose, adapter, platform)
        validate_file_map(record['files'])
        claimed = record['producer']
        if not isinstance(claimed, dict) or claimed.get('repository') != self.repository:
            raise ValueError('native provenance repository mismatch')
        producer = resolve_producer(self.repository, claimed['run'], claimed['attempt'],
                                    purpose, adapter, platform, self.authority)
        if claimed != producer:
            raise ValueError('native provenance producer claims mismatch')
        verify_signature(path, signature, producer)
        parents = record['parents']
        if (not isinstance(parents, list) or len(parents) > MAX_PARENTS
                or len(set(parents)) != len(parents)):
            raise ValueError('invalid native parent set')
        self.active.add(digest)
        parent_records = []
        for parent in parents:
            parent_path, parent_signature = parent_paths(store, parent)
            if digest_file(parent_path) != parent:
                raise ValueError('native parent record digest mismatch')
            parent_records.append(self.authenticate(parent_path, parent_signature, store, depth + 1))
        self.validate_lineage(record, parent_records)
        # Each record binds the exact retained parent-proof bytes. Ancestor
        # binary bytes were checked by the receiving producer, not duplicated.
        for name, facts in record['files'].items():
            if name.startswith(PARENTS + '/'):
                member = store / name.removeprefix(PARENTS + '/')
                raw_proof = bounded_read(member)
                if facts != {'bytes': len(raw_proof), 'sha256': hashlib.sha256(raw_proof).hexdigest()}:
                    raise ValueError('retained native parent proof mismatch')
        self.active.remove(digest)
        self.proofs[digest] = (record, path, signature)
        return record

    @staticmethod
    def validate_lineage(record, parents):
        purpose, adapter, platform = (record[k] for k in ('purpose', 'adapter', 'platform'))
        native = record['native']
        if purpose != 'assembly':
            if (not isinstance(native, dict) or set(native) != {'source_revision', 'inputs', 'toolchain'}
                    or not SHA.fullmatch(native['source_revision'])
                    or not isinstance(native['inputs'], dict) or not native['inputs']
                    or not isinstance(native['toolchain'], dict) or not native['toolchain']
                    or record['release_revision'] is not None):
                raise ValueError('invalid native build identity')
        if purpose == 'runtime':
            if parents:
                if (len(parents) != 1 or (parents[0]['purpose'], parents[0]['adapter'], parents[0]['platform']) !=
                        ('runtime', adapter, platform) or native != parents[0]['native']):
                    raise ValueError('runtime cache must preserve its authenticated native origin')
            elif native['source_revision'] != record['producer']['revision']:
                raise ValueError('fresh runtime provenance must bind its own native checkout')
        elif purpose == 'package' and platform != 'windows':
            if len(parents) > 1 or (parents and (
                    (parents[0]['purpose'], parents[0]['adapter'], parents[0]['platform']) !=
                    ('package', adapter, platform) or native != parents[0]['native'])):
                raise ValueError('Unix repackaging must preserve its original native identity')
        elif purpose == 'package':
            runtimes = [p for p in parents if p['purpose'] == 'runtime']
            sources = [p for p in parents if p['purpose'] == 'package' and p['platform'] in ('linux', 'macos')]
            if (len(parents) != 2 or len(runtimes) != 1 or len(sources) != 1
                    or any(p['adapter'] != adapter for p in parents)
                    or runtimes[0]['platform'] != 'windows' or native != runtimes[0]['native']):
                raise ValueError('Windows packaging requires its runtime and source parents')
        else:
            expected = NATIVE_TARGETS
            actual = [(p['adapter'], p['platform']) for p in parents if p['purpose'] == 'package']
            if (native is not None or not SHA.fullmatch(record['release_revision'] or '')
                    or len(actual) != len(parents) or len(actual) != len(expected) or set(actual) != expected):
                raise ValueError('assembly requires the complete authenticated package matrix')

    def verify(self, directory, purpose, adapter=None, platform=None, source=None, producer=None):
        self.proofs, self.active, self.proof_bytes = {}, set(), 0
        record_path, signature = directory / RECORD, directory / SIGNATURE
        record = self.authenticate(record_path, signature, directory / PARENTS)
        if (record['purpose'], record['adapter'], record['platform']) != (purpose, adapter, platform):
            raise ValueError('native payload purpose or matrix mismatch')
        if producer is not None and record['producer'] != producer:
            raise ValueError('downloaded native payload belongs to another producer')
        if source is not None:
            if purpose == 'assembly':
                if record['release_revision'] != command('git', '-C', source, 'rev-parse', 'HEAD'):
                    raise ValueError('native assembly release revision mismatch')
                selected = [self.proofs[parent][0] for parent in record['parents']]
            else:
                selected = [record]
            for item in selected:
                if item['native']['inputs'] != native_inputs(source, self.authority, item['adapter'], item['platform']):
                    raise ValueError('native package inputs differ from the approved release source')
        if payload_files(directory) != record['files']:
            raise ValueError('native payload files differ from authenticated manifest')
        digest = digest_file(record_path)
        expected_proofs = {f'{PARENTS}/{d}{suffix}' for d in self.proofs if d != digest
                           for suffix in ('.json', '.sigstore.json')}
        if {name for name in record['files'] if name.startswith(PARENTS + '/')} != expected_proofs:
            raise ValueError('native payload has missing or unreferenced parent proofs')
        return VerifiedPayload(directory, record, digest, dict(self.proofs))


def fetch(repository, run, purpose, adapter, platform, output, source=None, authority=ROOT, attempt=None):
    producer = resolve_producer(repository, run, attempt, purpose, adapter, platform, authority)
    download_payload(repository, producer, purpose, adapter, platform, output)
    return Verifier(repository, authority).verify(output, purpose, adapter, platform, source, producer)


def retain_parents(output, parents):
    store = output / PARENTS
    retained = []
    for parent in parents:
        store.mkdir(exist_ok=True)
        for digest, (_, record_path, signature) in parent.proofs.items():
            for source, target in zip((record_path, signature), parent_paths(store, digest)):
                data = bounded_read(source)
                if target.exists():
                    if bounded_read(target) != data:
                        raise ValueError('conflicting native lineage proof')
                else:
                    target.write_bytes(data)
        retained.append(VerifiedPayload(output, parent.record, parent.digest,
                         {digest: (record, *parent_paths(store, digest))
                          for digest, (record, _, _) in parent.proofs.items()}))
    return retained


def toolchain_identity():
    image = os.environ.get('ImageVersion')
    container = os.environ.get('NATIVE_CONTAINER')
    if container and not re.fullmatch(r'[^\s]+@sha256:[0-9a-f]{64}', container):
        raise ValueError('native container must be pinned by digest')
    if not (image or container) or os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted':
        raise ValueError('native provenance requires a hosted build image identity')
    value = {'os': os.environ['RUNNER_OS'], 'arch': os.environ['RUNNER_ARCH']}
    if image: value['image'] = image
    if container: value['container'] = container
    if os.environ.get('NATIVE_TOOLCHAIN'):
        value['native_toolchain'] = os.environ['NATIVE_TOOLCHAIN']
    for tool in ('cc', 'c++', 'cmake', 'dotnet'):
        if shutil.which(tool):
            value[tool] = subprocess.check_output([tool, '--version'], text=True, timeout=15).strip()[:16384]
    return value


def create_record(output, source, purpose, adapter=None, platform=None, parents=(), authority=ROOT):
    producer = current_producer(authority, purpose, adapter, platform)
    revision = admit_source(source, authority)
    if purpose == 'assembly':
        native = None
    elif parents:
        origin = next((p for p in parents if p.record['purpose'] == 'runtime'), parents[0])
        native = origin.record['native']
        if native['inputs'] != native_inputs(source, authority, adapter, platform):
            raise ValueError('repackaging inputs differ from original native build')
    else:
        native = {'source_revision': revision, 'inputs': native_inputs(source, authority, adapter, platform),
                  'toolchain': toolchain_identity()}
    retain_parents(output, parents)
    record = {'purpose': purpose, 'adapter': adapter, 'platform': platform, 'producer': producer,
              'native': native, 'parents': [p.digest for p in parents], 'files': payload_files(output),
              'release_revision': revision if purpose == 'assembly' else None}
    Verifier.validate_lineage(record, [p.record for p in parents])
    validate_file_map(record['files'])
    raw = canonical(record)
    if len(raw) > MAX_RECORD_BYTES:
        raise ValueError('native provenance record exceeds byte limit')
    with (output / RECORD).open('xb') as stream:
        stream.write(raw)
    return record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=('source', 'create', 'verify', 'fetch', 'pipeline-key'))
    parser.add_argument('--purpose', choices=('runtime', 'package', 'assembly'))
    parser.add_argument('--adapter')
    parser.add_argument('--platform', choices=('linux', 'macos', 'windows'))
    parser.add_argument('--source', type=Path)
    parser.add_argument('--directory', type=Path)
    parser.add_argument('--repo', default=os.environ.get('GITHUB_REPOSITORY'))
    parser.add_argument('--run')
    parser.add_argument('--attempt')
    parser.add_argument('--reuse-runtime', action='store_true')
    args = parser.parse_args()
    if args.operation == 'pipeline-key':
        print(hashlib.sha256(canonical(pipeline_blobs(ROOT))).hexdigest())
    elif args.operation == 'source':
        current_producer(ROOT, args.purpose, args.adapter, args.platform)
        admit_source(args.source.resolve(), ROOT)
    elif args.operation == 'create':
        parents = []
        if args.reuse_runtime:
            if args.purpose != 'runtime': raise ValueError('runtime cache requires runtime purpose')
            cached = Verifier(args.repo).verify(args.directory, 'runtime', args.adapter, args.platform, args.source)
            parents = retain_parents(args.directory, [cached])
            (args.directory / RECORD).unlink()
            (args.directory / SIGNATURE).unlink()
        create_record(args.directory, args.source.resolve(), args.purpose, args.adapter, args.platform, parents)
    elif args.operation == 'fetch':
        fetch(args.repo, args.run, args.purpose, args.adapter, args.platform, args.directory,
              args.source, attempt=args.attempt)
    else:
        Verifier(args.repo).verify(args.directory, args.purpose, args.adapter, args.platform, args.source)


if __name__ == '__main__':
    main()
