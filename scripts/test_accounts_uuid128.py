#!/usr/bin/env python3
"""Full-schema PostgreSQL cutover regression. REMIND_TEST_POSTGRES_URL is required."""
import os
from pathlib import Path
import unittest
import uuid
from urllib.parse import urlsplit,urlunsplit

import psycopg
from psycopg import sql
from psycopg.types.json import Jsonb
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from accounts_uuid128 import migrate
from migrate_account_uuids import manifest_for

ROOT=Path(__file__).resolve().parents[1]

class Backfill(unittest.TestCase):
    def setUp(self):
        self.admin=os.environ["REMIND_TEST_POSTGRES_URL"]
        self.name="remind_uuid_test_"+uuid.uuid4().hex
        with psycopg.connect(self.admin,autocommit=True) as conn:
            conn.execute(sql.SQL("CREATE DATABASE {}").format(sql.Identifier(self.name)))
        parsed=urlsplit(self.admin)
        self.url=urlunsplit((parsed.scheme,parsed.netloc,"/"+self.name,parsed.query,parsed.fragment))
        self.mapping={"Ab1":str(uuid.uuid4()),"Si2":str(uuid.uuid4()),"De3":str(uuid.uuid4())}
        self.kinds={"Ab1":"carbon","Si2":"silicon","De3":"carbon"}
        self.key=os.urandom(32)

    def tearDown(self):
        with psycopg.connect(self.admin,autocommit=True) as conn:
            conn.execute(sql.SQL("DROP DATABASE {} WITH (FORCE)").format(sql.Identifier(self.name)))

    def migrations(self,folder,schema="public"):
        with psycopg.connect(self.url) as conn:
            for path in sorted((ROOT/folder).glob("*.sql")):
                with conn.transaction():
                    conn.execute(sql.SQL("SET LOCAL search_path TO {}").format(sql.Identifier(schema)))
                    conn.execute(path.read_text(),prepare=False)

    def seed(self,schema="public"):
        owner,resource,dest=(uuid.uuid4() for _ in range(3))
        nonce=os.urandom(12)
        aad=b"remind-destination:v2\0"+dest.bytes+b"\0"+owner.bytes+b"\0signing_secret"
        encrypted=AESGCM(self.key).encrypt(nonce,b"preserved provider signing secret",aad)
        with psycopg.connect(self.url) as conn:
            conn.execute(sql.SQL("SET LOCAL search_path TO {}").format(sql.Identifier(schema)))
            conn.execute("INSERT INTO accounts(uuid,kind,public_id) VALUES('Ab1','carbon','c:ada'),('Si2','silicon','si:scout'),('De3',NULL,'')")
            conn.execute("UPDATE accounts SET custodian_uuid='Ab1' WHERE uuid='Si2'")
            conn.execute("INSERT INTO account_keys(storage_id,account_uuid,origin) VALUES(%s,'Si2','accounts')",(owner,))
            conn.execute("INSERT INTO schedules(id,owner_principal_id,silicon_id,reminder_text,schedule_kind,cron_expression) VALUES(%s,%s,'si:scout','Ab1 remains prose','recurring','0 9 * * *')",(resource,owner))
            conn.execute("INSERT INTO hook_destinations(id,owner_principal_id,silicon_id,endpoint_url_ciphertext,endpoint_url_nonce,signing_secret_ciphertext,signing_secret_nonce,encryption_key_version) VALUES(%s,%s,'si:scout',%s,%s,%s,%s,1)",(dest,owner,b'unchanged-url-cipher',nonce,encrypted,nonce))
            conn.execute("INSERT INTO reminder_viewers(id,owner_uuid,viewer_uuid,granted_by_uuid) VALUES(gen_random_uuid(),'Si2','Ab1','Ab1')")
            conn.execute("INSERT INTO silicon_allowances(id,silicon_uuid,allowed_uuid,created_by_uuid) VALUES(gen_random_uuid(),'Si2','Ab1','Ab1')")
            conn.execute("INSERT INTO audit_records(id,actor_type,actor_id,action,resource_type,metadata) VALUES(gen_random_uuid(),'carbon','Ab1','fixture','account',%s),(gen_random_uuid(),'service','Ab1','fixture','account','{}')",(Jsonb({"subject_uuid":"Si2","text":"Ab1"}),))
        return (resource,owner,dest,nonce,encrypted,aad)

    def check(self,schema,fixture):
        resource,owner,dest,nonce,encrypted,aad=fixture
        with psycopg.connect(self.url) as conn:
            conn.execute(sql.SQL("SET LOCAL search_path TO {}").format(sql.Identifier(schema)))
            self.assertEqual(conn.execute("SELECT account_uuid FROM account_keys WHERE storage_id=%s",(owner,)).fetchone()[0],self.mapping["Si2"])
            self.assertEqual(conn.execute("SELECT id,owner_principal_id,silicon_id,reminder_text FROM schedules").fetchone(),(resource,owner,'si:scout','Ab1 remains prose'))
            sealed=conn.execute("SELECT id,owner_principal_id,signing_secret_nonce,signing_secret_ciphertext FROM hook_destinations").fetchone()
            self.assertEqual(sealed,(dest,owner,nonce,encrypted))
            self.assertEqual(AESGCM(self.key).decrypt(sealed[2],sealed[3],aad),b"preserved provider signing secret")
            self.assertEqual(conn.execute("SELECT custodian_uuid FROM accounts WHERE uuid=%s",(self.mapping["Si2"],)).fetchone()[0],self.mapping["Ab1"])
            self.assertEqual(conn.execute("SELECT count(*) FROM reminder_viewers WHERE revoked_at IS NULL").fetchone()[0],1,"identity rewrite is not a custody transfer")
            self.assertEqual(conn.execute("SELECT actor_id FROM audit_records WHERE actor_type='service'").fetchone()[0],"Ab1","unrelated service IDs are not accounts")
            self.assertEqual(conn.execute("SELECT metadata FROM audit_records WHERE actor_type='carbon'").fetchone()[0],{"subject_uuid":self.mapping["Si2"],"text":"Ab1"})
            self.assertEqual(conn.execute("SELECT count(*) FROM accounts_uuid128_map").fetchone()[0],3)
            with self.assertRaises(psycopg.Error):
                conn.execute("UPDATE audit_records SET action='mutated'")

    def test_production_dry_apply_replay_and_collisions(self):
        self.migrations("migrations")
        fixture=self.seed()
        with psycopg.connect(self.url) as conn:
            manifest=manifest_for(conn)
        migrate(self.url,manifest,self.mapping,self.kinds,"f"*64,False)
        with psycopg.connect(self.url) as conn:
            self.assertEqual(conn.execute("SELECT count(*) FROM accounts WHERE uuid='Si2'").fetchone()[0],1)
            self.assertEqual(conn.execute("SELECT count(*) FROM accounts_uuid128_map").fetchone()[0],0)
        migrate(self.url,manifest,self.mapping,self.kinds,"f"*64,True)
        self.check("public",fixture)
        replay=migrate(self.url,manifest,self.mapping,self.kinds,"f"*64,True)
        self.assertFalse(replay["changed"])
        conflict=dict(self.mapping);conflict["Ab1"]=str(uuid.uuid4())
        with self.assertRaises(ValueError):migrate(self.url,manifest,conflict,self.kinds,"e"*64,True)

    def test_all_active_and_retained_test_schemas_migrate_atomically(self):
        self.migrations("testing/migrations")
        fixtures={}
        for retired in [False,True]:
            env=uuid.uuid4(); schema="remind_test_"+env.hex
            with psycopg.connect(self.url) as conn:
                conn.execute(sql.SQL("CREATE SCHEMA {}").format(sql.Identifier(schema)))
                conn.execute("INSERT INTO public.testing_environments(id,creator_id,owner_uuid,name,key_hash,secrets,deleted_at,purge_after) VALUES(%s,'Ab1','Ab1',%s,%s,%s,CASE WHEN %s THEN now() END,CASE WHEN %s THEN now()+interval '45 days' END)",(env,schema,None if retired else os.urandom(32),Jsonb({"ciphertext":"preserved"}),retired,retired))
            self.migrations("migrations",schema)
            fixtures[schema]=self.seed(schema)
        with psycopg.connect(self.url) as conn:manifest=manifest_for(conn,True)
        migrate(self.url,manifest,self.mapping,self.kinds,"f"*64,False)
        with psycopg.connect(self.url) as conn:self.assertEqual(conn.execute("SELECT count(*) FROM public.testing_environments WHERE owner_uuid='Ab1'").fetchone()[0],2)
        migrate(self.url,manifest,self.mapping,self.kinds,"f"*64,True)
        for schema,fixture in fixtures.items():self.check(schema,fixture)
        with psycopg.connect(self.url) as conn:
            rows=conn.execute("SELECT owner_uuid,creator_id,secrets FROM public.testing_environments").fetchall()
            self.assertEqual(rows,[(self.mapping["Ab1"],self.mapping["Ab1"],{"ciphertext":"preserved"})]*2)
        self.assertFalse(migrate(self.url,manifest,self.mapping,self.kinds,"f"*64,True)["changed"])

if __name__=="__main__":unittest.main()
