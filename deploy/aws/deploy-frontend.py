#!/usr/bin/env python3
"""Install a digest-pinned frontend on the standalone host through SSM."""
import argparse
import json
import os
from pathlib import Path
import shlex
import subprocess
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('image')
parser.add_argument('--instance', default='i-0546693fac4a32d6d')
args = parser.parse_args()
aws = ['aws', '--profile', os.environ.get('AWS_PROFILE', 'silicon-production'),
       '--region', os.environ.get('AWS_REGION', 'us-east-1')]
script = Path(__file__).with_name('install-frontend.sh').read_text()
commands = 'bash -s -- ' + shlex.quote(args.image) + " <<'INSTALL_FRONTEND'\n" + script + '\nINSTALL_FRONTEND'
request = {'DocumentName': 'AWS-RunShellScript', 'InstanceIds': [args.instance],
           'TimeoutSeconds': 600, 'Parameters': {'commands': [commands]}}
command = json.loads(subprocess.check_output(
    aws + ['ssm', 'send-command', '--cli-input-json', json.dumps(request)], text=True))['Command']['CommandId']
print('SSM deployment:', command, flush=True)
for _ in range(90):
    time.sleep(5)
    result = subprocess.run(aws + ['ssm', 'get-command-invocation', '--command-id', command,
                            '--instance-id', args.instance], capture_output=True, text=True)
    if result.returncode:
        if 'InvocationDoesNotExist' in result.stderr:
            continue
        raise SystemExit(result.stderr)
    status = json.loads(result.stdout)
    if status['Status'] in ('Pending', 'InProgress', 'Delayed'):
        continue
    print(status.get('StandardOutputContent', ''))
    if status['Status'] != 'Success':
        raise SystemExit(status.get('StandardErrorContent') or status['Status'])
    break
else:
    raise SystemExit('Deployment still running. Inspect the SSM command before retrying.')
