"""The final remote release inventory participates in publication admission."""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from publish_release import check_assets, publish, release_assets


REVISION = 'a' * 40


class PublishTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.paths = [root / name for name in ('native.zip', 'SHA256SUMS')]
        for path in self.paths:
            path.write_bytes(path.name.encode())
        self.asset_list = root / 'assets.json'
        self.asset_list.write_text(json.dumps([str(p) for p in self.paths]))
        self.assets, self.calls = [], []
        self.release = dict(id=7, draft=True, tag_name='1.0.0')
        self.after_upload = lambda: None

    def asset(self, path, identity=1):
        return dict(id=identity, name=path.name, state='uploaded', size=path.stat().st_size,
                    digest='sha256:' + hashlib.sha256(path.read_bytes()).hexdigest())

    def github(self, path):
        if '/assets?' in path:
            return list(self.assets)
        if '/git/ref/' in path:
            return dict(object=dict(type='commit', sha=REVISION))
        if '/releases?' in path:
            return [dict(self.release)]
        if path.endswith('/releases/7'):
            return dict(self.release)
        raise AssertionError('unexpected API request: ' + path)

    def command(self, args, **_):
        self.calls.append(args)
        if args[1:3] == ['release', 'upload']:
            self.assets.extend(self.asset(Path(p), len(self.assets) + i + 1)
                               for i, p in enumerate(args[6:]))
            self.after_upload()

    def run_publish(self):
        with patch('publish_release.github', side_effect=self.github), \
                patch('publish_release.subprocess.run', side_effect=self.command):
            publish('owner/repo', '1.0.0', REVISION, self.asset_list)

    def test_existing_extra_asset_prevents_upload_and_publication(self):
        extra = self.asset(self.paths[0])
        extra['name'] = 'unverified-native.zip'
        self.assets = [extra]
        with self.assertRaisesRegex(ValueError, 'unexpected'):
            self.run_publish()
        self.assertEqual(self.calls, [])

    def test_empty_draft_uploads_and_publishes_exact_inventory(self):
        self.run_publish()
        self.assertEqual(len(self.calls), 2)
        self.assertIn('draft=false', self.calls[-1])
        self.assertIn('repos/owner/repo/releases/7', self.calls[-1])
        self.assertEqual({a['name'] for a in self.assets}, {p.name for p in self.paths})

    def test_matching_partial_upload_resumes_without_reuploading(self):
        self.assets = [self.asset(self.paths[0])]
        self.run_publish()
        self.assertNotIn(str(self.paths[0]), self.calls[0])
        self.assertIn(str(self.paths[1]), self.calls[0])

    def test_complete_matching_draft_only_publishes(self):
        self.assets = [self.asset(p, i + 1) for i, p in enumerate(self.paths)]
        self.run_publish()
        self.assertEqual(len(self.calls), 1)
        self.assertIn('PATCH', self.calls[0])

    def test_added_or_modified_asset_after_upload_prevents_publication(self):
        for change in ('extra', 'digest', 'missing', 'published', 'tag'):
            with self.subTest(change=change):
                self.assets, self.calls = [], []
                self.release.update(draft=True, tag_name='1.0.0')
                def mutate():
                    if change == 'extra': self.assets.append(dict(id=99, name='extra.zip'))
                    elif change == 'digest': self.assets[0]['digest'] = 'sha256:' + '0' * 64
                    elif change == 'missing': self.assets.pop()
                    elif change == 'published': self.release['draft'] = False
                    else: self.release['tag_name'] = 'other'
                self.after_upload = mutate
                with self.assertRaises(ValueError): self.run_publish()
                self.assertEqual(len(self.calls), 1)
                self.assertNotIn('PATCH', self.calls[0])

    def test_asset_identity_state_size_and_digest_are_required(self):
        original = self.asset(self.paths[0])
        expected = {original['name']: (original['size'], original['digest'])}
        for key, value in [('size', 999), ('digest', None), ('state', 'starter')]:
            with self.subTest(key=key), self.assertRaises(ValueError):
                check_assets([dict(original, **{key: value})], expected, complete=True)
        with self.assertRaises(ValueError): check_assets([original, original], expected, True)

    def test_full_remote_inventory_is_paginated(self):
        pages = [[dict(id=i) for i in range(100)], [dict(id=101)]]
        with patch('publish_release.github', side_effect=pages) as api:
            self.assertEqual(len(release_assets('owner/repo', 7)), 101)
        self.assertIn('page=2', api.call_args.args[0])


if __name__ == '__main__': unittest.main()
