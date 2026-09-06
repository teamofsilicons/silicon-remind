#!/usr/bin/env python3
"""Refresh the public guides included in both registry packages before packaging."""
from pathlib import Path
import shutil
root = Path(__file__).resolve().parents[1]
for package in ['client', 'cli']:
    destination = root / 'crates' / package
    for group in ['api', 'client', 'cli']:
        shutil.copytree(root / 'docs' / group, destination / 'docs' / group, dirs_exist_ok=True)
    for name in ['iam.md', 'testing-environments.md', 'hook-delivery.md']:
        shutil.copyfile(root / 'docs' / name, destination / 'docs' / name)
    shutil.copyfile(root / 'openapi.yaml', destination / 'openapi.yaml')
