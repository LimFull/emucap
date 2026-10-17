#!/usr/bin/env python3
"""Exercise the maintained native source admission with a local shallow origin."""
from pathlib import Path
import os
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]

def git(*args):
    return subprocess.check_output(['git', *map(str, args)], text=True).strip()

for adapter, stop_marker, checkout_name in (
    ('dolphin', '# These top-level submodules', 'dolphin-src'),
    ('pcsx2', 'git -C "$SRC" checkout -- \\', 'pcsx2'),
):
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        upstream = root / 'upstream'
        git('init', '-q', upstream)
        git('-C', upstream, 'config', 'user.name', 'Fixture')
        git('-C', upstream, 'config', 'user.email', 'fixture@example.invalid')
        (upstream / 'source.txt').write_text('locked source\n')
        git('-C', upstream, 'add', '.')
        git('-C', upstream, 'commit', '-qm', 'fixture')
        revision = git('-C', upstream, 'rev-parse', 'HEAD')
        origin = root / 'origin'
        git('clone', '-q', '--depth=1', upstream.as_uri(), origin)
        assert git('-C', origin, 'rev-parse', '--is-shallow-repository') == 'true'
        (origin / 'source.txt').write_text('operator edits\n')
        before = git('-C', origin, 'status', '--porcelain')
        recipe = root / f'adapters/{adapter}'
        recipe.mkdir(parents=True)
        common = recipe.parent / '_common'
        common.mkdir()
        (common / 'build-lock.sh').write_bytes((ROOT / 'adapters/_common/build-lock.sh').read_bytes())
        prefix = (ROOT / f'adapters/{adapter}/build.sh').read_text().split(stop_marker)[0]
        script = recipe / 'build.sh'
        script.write_text(prefix)
        (recipe / 'patches').mkdir()
        (recipe / 'upstream.lock').write_text(
            f'{adapter.upper()}_REPO={origin}\n{adapter.upper()}_COMMIT={revision}\n'
            f'{adapter.upper()}_PATCHSET_SHA256=pending\n')
        work = root / 'work'
        env = dict(os.environ, **{f'EMUCAP_{adapter.upper()}_SRC':str(origin),
                                f'EMUCAP_{adapter.upper()}_WORK':str(work)})
        subprocess.run(['bash', str(script)], env=env, check=True, timeout=30)
        checkout = work / checkout_name
        assert git('-C', checkout, 'rev-parse', 'HEAD') == revision
        assert (checkout / 'source.txt').read_text() == 'locked source\n'
        promisor = subprocess.run(['git', '-C', str(checkout), 'config', '--get',
                                   'remote.origin.promisor'], capture_output=True, text=True)
        assert promisor.stdout.strip() != 'true'
        assert git('-C', origin, 'status', '--porcelain') == before
        assert (origin / 'source.txt').read_text() == 'operator edits\n'
        subprocess.run(['bash', str(script)], env=env, check=True, timeout=30)
        assert git('-C', checkout, 'rev-parse', 'HEAD') == revision
print('Dolphin and PCSX2 pinned source checkout passed')
