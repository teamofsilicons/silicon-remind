#!/usr/bin/env python3
"""Read only Remind receipt metadata through the restricted runtime DB role."""
import json
import os
import subprocess
import sys
import urllib.parse
import boto3

try:
    raw = boto3.client('secretsmanager').get_secret_value(SecretId=os.environ['RUNTIME_SECRET_ARN'])
    config = json.loads(raw['SecretString'])
    uri = urllib.parse.urlsplit(config['REMIND_DATABASE_URL'])
    query = urllib.parse.parse_qs(uri.query)
    env = dict(os.environ, PGHOST=uri.hostname, PGPORT=str(uri.port or 5432),
               PGUSER=urllib.parse.unquote(uri.username), PGPASSWORD=urllib.parse.unquote(uri.password),
               PGDATABASE=uri.path.lstrip('/'), PGSSLMODE='verify-full',
               PGSSLROOTCERT=query['sslrootcert'][0])
    sql = '''SELECT json_build_object('event_id',event_id,'type',event_type,
             'status',status,'received_at',received_at,'processed_at',processed_at)
             FROM internal_event_receipts ORDER BY received_at DESC LIMIT 10'''
    result = subprocess.run(['psql','-X','-t','-A','--set','ON_ERROR_STOP=1','--command',sql],
                            env=env, capture_output=True, text=True)
    if result.returncode:
        print('Receipt inspection query failed', file=sys.stderr)
        sys.exit(1)
    print(result.stdout, end='')
    print('Receipt inspection complete')
except Exception:
    print('Receipt inspection failed', file=sys.stderr)
    sys.exit(1)
