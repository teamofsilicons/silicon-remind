#!/usr/bin/env python3
"""Bundle only the Apache-2.0 public packages for the documentation installer."""
from pathlib import Path
import hashlib, shutil, subprocess, tarfile, tempfile
root = Path(__file__).resolve().parents[1]
output = root / 'docs-site/dist'
with tempfile.TemporaryDirectory(prefix='remind-public-source-') as work:
    stage = Path(work) / 'remind-client-source'
    stage.mkdir()
    (stage / 'Cargo.toml').write_text('[workspace]\nmembers = ["crates/client", "crates/cli"]\nresolver = "3"\n')
    shutil.copyfile(root / 'Cargo.lock', stage / 'Cargo.lock')
    for package in ['client', 'cli']:
        destination = stage / 'crates' / package
        destination.mkdir(parents=True)
        for name in ['src', 'docs', 'Cargo.toml', 'README.md', 'LICENSE', 'openapi.yaml']:
            source = root / 'crates' / package / name
            if source.is_dir(): shutil.copytree(source, destination / name)
            else: shutil.copyfile(source, destination / name)
    subprocess.run(['cargo', 'generate-lockfile', '--offline', '--manifest-path', str(stage / 'Cargo.toml')], check=True, capture_output=True)
    archive = output / 'remind-client-source.tar.gz'
    with tarfile.open(archive, 'w:gz') as tar:
        tar.add(stage, arcname=stage.name)
    (output / 'remind-client-source.sha256').write_text(hashlib.sha256(archive.read_bytes()).hexdigest() + '  remind-client-source.tar.gz\n')
print('Packaged public CLI/client source; backend source and credentials excluded')
