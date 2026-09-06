#!/usr/bin/env python3
"""Migrate both databases and publish restricted runtime credentials, without logging secrets."""
import json
import os
import subprocess
import sys
import urllib.parse
import boto3

CA = '/opt/silicon-remind/aws-rds-global-bundle.pem'

def database_url(user, password, host, database):
    return ('postgresql://' + urllib.parse.quote(user, safe='') + ':'
            + urllib.parse.quote(password, safe='') + '@' + host + ':5432/'
            + database + '?sslmode=verify-full&sslrootcert=' + CA)

def command(args, env, sql=None):
    result = subprocess.run(args, input=sql, text=True, env=env, capture_output=True)
    if result.returncode:
        raise RuntimeError(args[0] + ' failed with exit ' + str(result.returncode))

def db_env(master, host, database):
    return dict(os.environ, PGHOST=host, PGPORT='5432', PGDATABASE=database,
                PGUSER=master['username'], PGPASSWORD=master['password'],
                PGSSLMODE='verify-full', PGSSLROOTCERT=CA)

def configure(master, host, database, role, password, testing=False):
    env = dict(db_env(master, host, database), REMIND_ROLE_PASSWORD=password)
    sql = r"""\getenv role_password REMIND_ROLE_PASSWORD
BEGIN;
DO $roles$
BEGIN
  IF pg_catalog.to_regrole('%s') IS NULL THEN
    CREATE ROLE %s LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOINHERIT NOBYPASSRLS;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname='%s' AND rolcanlogin
      AND NOT (rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls OR rolinherit)) THEN
    RAISE EXCEPTION 'Unexpected runtime role attributes';
  END IF;
END;
$roles$;
ALTER ROLE %s PASSWORD :'role_password';
REVOKE ALL ON DATABASE %s FROM PUBLIC;
GRANT CONNECT ON DATABASE %s TO %s;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
""" % (role, role, role, role, database, database, role)
    if testing:
        # Test runtime owns its control tables and replica schemas because its
        # public lifecycle operations legitimately create/drop isolated schemas.
        sql += 'GRANT CREATE ON DATABASE '+database+' TO '+role+';\n'
        sql += 'GRANT USAGE, CREATE ON SCHEMA public TO '+role+';\n'
    command(['psql', '-X', '--set', 'ON_ERROR_STOP=1'], env, sql+'COMMIT;\n')
    print('Configured restricted role: '+role, flush=True)

def main():
    sm=boto3.client('secretsmanager')
    def secret(arn):return json.loads(sm.get_secret_value(SecretId=arn)['SecretString'])
    app=secret(os.environ['APP_SECRET_ARN'])
    keys=['REMIND_IAM_APP_SECRET','REMIND_IAM_WEBHOOK_KEYRING','REMIND_ENCRYPTION_KEYRING','REMIND_INTERNAL_API_TOKEN']
    for k in keys+['REMIND_RUNTIME_DATABASE_PASSWORD','REMIND_TEST_DATABASE_PASSWORD']:
        if not isinstance(app.get(k),str) or not app[k] or any(c in app[k] for c in '\r\n\0'):
            raise RuntimeError('Missing or invalid secret field: '+k)
    runtime={k:app[k] for k in keys}
    runtime['REMIND_DATABASE_URL']=database_url('remind_runtime',app['REMIND_RUNTIME_DATABASE_PASSWORD'],os.environ['DB_HOST'],'silicon_remind')
    runtime['REMIND_TEST_DATABASE_URL']=database_url('remind_testing',app['REMIND_TEST_DATABASE_PASSWORD'],os.environ['TEST_DB_HOST'],'silicon_remind_test')
    metadata=sm.describe_secret(SecretId=os.environ['RUNTIME_SECRET_ARN'])
    if metadata.get('VersionIdsToStages'):
        current=secret(os.environ['RUNTIME_SECRET_ARN'])
        for k in ['REMIND_DATABASE_URL','REMIND_TEST_DATABASE_URL','REMIND_ENCRYPTION_KEYRING']:
            if current.get(k)!=runtime[k]:raise RuntimeError('Bootstrap cannot rotate '+k)
    master=secret(os.environ['DB_SECRET_ARN']);testmaster=secret(os.environ['TEST_DB_SECRET_ARN'])
    configure(master,os.environ['DB_HOST'],'silicon_remind','remind_runtime',app['REMIND_RUNTIME_DATABASE_PASSWORD'])
    configure(testmaster,os.environ['TEST_DB_HOST'],'silicon_remind_test','remind_testing',app['REMIND_TEST_DATABASE_PASSWORD'],testing=True)
    migration=dict(os.environ,REMIND_ENVIRONMENT='production',
        REMIND_MIGRATOR_DATABASE_URL=database_url(master['username'],master['password'],os.environ['DB_HOST'],'silicon_remind'),
        REMIND_TEST_MIGRATOR_DATABASE_URL=runtime['REMIND_TEST_DATABASE_URL'],
        REMIND_MIGRATOR_DATABASE_STATEMENT_TIMEOUT_SECONDS='300')
    command(['/usr/local/bin/remind-migrate'],migration)
    command(['psql','-X','--set','ON_ERROR_STOP=1','--file','/opt/silicon-remind/runtime-grants.sql'],db_env(master,os.environ['DB_HOST'],'silicon_remind'))
    sm.put_secret_value(SecretId=os.environ['RUNTIME_SECRET_ARN'],SecretString=json.dumps(runtime))
    print('Both databases migrated; restricted runtime secret published',flush=True)

if __name__=='__main__':
    try:main()
    except Exception as error:
        print('Remind bootstrap failed: '+(str(error) if type(error) is RuntimeError else type(error).__name__),file=sys.stderr,flush=True)
        sys.exit(1)
