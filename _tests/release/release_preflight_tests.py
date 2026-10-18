"""A failed publisher does not invalidate successful core jobs; failed producers do."""
import unittest
from unittest.mock import patch
from release_preflight import check_core_run


class CoreReuseTests(unittest.TestCase):
    def setUp(self):
        self.run = dict(repository={'full_name': 'owner/repo'}, event='workflow_dispatch',
                        head_branch='main', path='.github/workflows/release-core.yml',
                        status='completed', conclusion='failure', run_attempt=2)
        self.jobs = [dict(name=f'build / build (runner, {t}, 4)', status='completed', conclusion='success')
                     for t in ('aarch64-apple-darwin', 'x86_64-unknown-linux-gnu', 'x86_64-pc-windows-msvc')]

    def admit(self):
        with patch('release_preflight.github', return_value=self.run), \
                patch('release_preflight.github_pages', return_value=self.jobs) as pages:
            check_core_run('owner/repo', 12)
            self.assertEqual(pages.call_args.args[0], 'repos/owner/repo/actions/runs/12/attempts/2/jobs')

    def test_failed_publisher_keeps_completed_core_jobs_reusable(self):
        self.jobs.append(dict(name='publish', status='completed', conclusion='failure'))
        self.admit()

    def test_failed_missing_duplicate_or_incomplete_producer_is_rejected(self):
        original = list(self.jobs)
        for case in ('failure', 'missing', 'duplicate', 'in_progress'):
            with self.subTest(case=case):
                self.jobs = [dict(j) for j in original]
                if case == 'missing': self.jobs.pop()
                elif case == 'duplicate': self.jobs.append(self.jobs[0])
                elif case == 'failure': self.jobs[0]['conclusion'] = 'failure'
                else: self.jobs[0]['status'] = 'in_progress'
                with self.assertRaises(ValueError): self.admit()

    def test_foreign_repository_branch_event_and_workflow_are_rejected(self):
        original = dict(self.run)
        for key, value in [('repository', {'full_name':'other/repo'}), ('event','pull_request'),
                           ('head_branch','untrusted'), ('path','.github/workflows/other.yml'),
                           ('status','in_progress')]:
            self.run = dict(original, **{key: value})
            with self.subTest(key=key), self.assertRaises(ValueError): self.admit()


if __name__ == '__main__':
    unittest.main()
