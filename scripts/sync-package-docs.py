#!/usr/bin/env python3
"""Copy the public guides and openapi.yaml into both registry packages (crates/client, crates/cli).

The CLI compiles these copies into `remind docs <topic>`, and both crates ship them on crates.io.
Run it after editing docs/; `--check` changes nothing and exits 1 when a copy is stale or a file
that is no longer published is still there (CI runs the plain form followed by `git diff`).
"""
from pathlib import Path
import filecmp
import shutil
import sys

ROOT = Path(__file__).resolve().parents[1]
GROUPS = ['api', 'client', 'cli']
GUIDES = [
    'accounts.md',
    'testing-environments.md',
    'webhook-delivery.md',
    'version-policy.md',
    'browser.md',
    'diagnostics.md',
    'releases.md',
]
RETIRED = ['iam.md', 'honeycomb-lifecycle.md']


def pairs(package: str):
    destination = ROOT / 'crates' / package
    for group in GROUPS:
        for source in sorted((ROOT / 'docs' / group).rglob('*')):
            if source.is_file():
                yield source, destination / 'docs' / group / source.relative_to(ROOT / 'docs' / group)
    for name in GUIDES:
        yield ROOT / 'docs' / name, destination / 'docs' / name
    yield ROOT / 'openapi.yaml', destination / 'openapi.yaml'


def main() -> int:
    check = '--check' in sys.argv[1:]
    stale = []
    for package in ['client', 'cli']:
        for source, target in pairs(package):
            if not target.exists() or not filecmp.cmp(source, target, shallow=False):
                stale.append(target)
                if not check:
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(source, target)
        for name in RETIRED:
            target = ROOT / 'crates' / package / 'docs' / name
            if target.exists():
                stale.append(target)
                if not check:
                    target.unlink()
    if check and stale:
        for target in stale:
            print(f'stale: {target.relative_to(ROOT)}', file=sys.stderr)
        print('Run python3 scripts/sync-package-docs.py to refresh the package copies.', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
