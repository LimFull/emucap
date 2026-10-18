"""Admit an existing draft and optional completed core producer before expensive work."""
import argparse
import re

from native_producer import github, github_pages, positive_id, repository_name
from publish_release import check_draft, find_release


def check_core_run(repo, run_id):
    run_id = positive_id(run_id)
    run = github(f'repos/{repo}/actions/runs/{run_id}')
    if (run['repository']['full_name'] != repo or run['event'] != 'workflow_dispatch'
            or run['head_branch'] not in ('main', 'next')
            or run['path'] != '.github/workflows/release-core.yml'
            or run['status'] != 'completed'):
        raise ValueError('core reuse requires a completed public release workflow')
    attempt = positive_id(run['run_attempt'])
    jobs = github_pages(f'repos/{repo}/actions/runs/{run_id}/attempts/{attempt}/jobs', 'jobs')
    targets = ('aarch64-apple-darwin', 'x86_64-unknown-linux-gnu', 'x86_64-pc-windows-msvc')
    for target in targets:
        selected = [j for j in jobs if re.fullmatch(
            r'build / build \([^,]+, ' + re.escape(target) + r', [0-9]+\)', j['name'])]
        if len(selected) != 1 or selected[0]['conclusion'] != 'success' or selected[0]['status'] != 'completed':
            raise ValueError('core producing job did not complete successfully: ' + target)
    # Source/version/archive identity is checked again against downloaded bytes by core_assets.
    return run


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('repo', 'tag', 'revision'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--core-run')
    args = parser.parse_args()
    repository_name(args.repo)
    check_draft(args.repo, find_release(args.repo, args.tag), args.tag, args.revision)
    if args.core_run:
        check_core_run(args.repo, args.core_run)


if __name__ == '__main__':
    main()
