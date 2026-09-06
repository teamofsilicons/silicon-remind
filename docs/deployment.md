# Production deployment and releases

Remind uses the `silicon-production` AWS profile, account `234951665042`, region
`us-east-1`. Its dedicated CloudFormation stack is `silicon-remind-production`.
The public origin is `https://backend.remind.teamofsilicons.com`, with the IAM
callback at `/webhook/`. The stack reuses the existing production VPC/subnets,
but owns separate services, databases, security groups, logs, load balancer and
rate-limiting WAF. It does not deploy any other Silicon product.

## Runtime and data

[`deploy/aws/production.yaml`](../deploy/aws/production.yaml) defines private ARM64
Fargate API and worker tasks, initially one each at 0.5 vCPU and 2 GiB. Only the
API is reachable through the load balancer. Public forwarding allows `/api/v1/*`,
`/webhook`, `/webhook/` and `/health/*`; internal routes and metrics remain private.
HTTPS uses ACM, and HTTP redirects to HTTPS. Container root filesystems are
read-only, UID/GID is 10001, and Linux capabilities are dropped.

Separate encrypted RDS PostgreSQL 17 instances hold production and test data.
Production uses `db.t4g.small`, testing `db.t4g.micro`; these are initial,
single-AZ sizes. Production backups retain seven days, testing one day. Both
have deletion protection and snapshot retention on replacement/deletion.
Their network endpoints are private; TLS uses hostname and RDS CA verification.
Maintain database and encryption-key backups together.

Application credentials and independent production encryption/internal secrets
are held in Secrets Manager `silicon-remind/production`. Bootstrap writes the
restricted data-plane configuration to `silicon-remind/runtime-production`.
Runtime execution roles can inject only that restricted secret; database master
secrets are available only to the one-off bootstrap role. No local test database
or local test encryption key is deployed.

The production runtime role can operate data and read migration history but
cannot change schemas. The test runtime role owns its control tables and replica
schemas, because test lifecycle operations legitimately create, clean and drop
those schemas. It has no database-creation, role-creation, superuser or RLS-bypass
rights. All test replicas use the same embedded migrations as production.

## Release procedure

1. Build the root runtime image from reviewed source. Wrap it with
   `deploy/aws/Dockerfile.runtime` to install the current AWS RDS CA bundle.
   Build `deploy/aws/Dockerfile.bootstrap` from that CA-bearing runtime.
2. Push unique, immutable tags to ECR `silicon-remind-production`; resolve both
   digests and place digest-pinned URIs in the CloudFormation parameters.
3. Validate the template with AWS and `cfn-lint`. Create and inspect a change set
   scoped to this stack. Initial creation uses `RuntimeDesiredCount=0`.
4. Run the stack's `BootstrapTaskDefinitionArn` once in its private subnets and
   `TaskSecurityGroupId`. Inspect the specific stopped task and bootstrap log.
   Success means both databases migrated, production grants applied and the
   restricted runtime secret published. Bootstrap refuses silent credential or
   encryption-key changes on reruns; rotation needs a coordinated procedure.
5. Set `RuntimeDesiredCount=1` through a reviewed stack update after successful
   bootstrap. Check API/worker container health, ALB target health and public
   `/health/ready`, then authenticate a real IAM session and exercise a sandbox.
6. Confirm the intended pending IAM webhook URL. Obtain a fresh direct-Carbon
   step-up for action `application.webhook.approve` and the internal app UUID,
   then run `iam app approve-webhook 'tos>remind'`. Check IAM metadata and an
   actual upstream delivery, not merely a manually signed fixture.

For updates, preserve all existing stack parameters unless explicitly changing
one. Inspect proposed replacements. Use normal rollback for task-definition
updates; disable-rollback does not support replacement changes. Database changes
are forward-only; rolling back a task image does not undo migrations. Do not
remove database deletion protection or purge retained secrets as routine cleanup.

## DNS

Namecheap manages `teamofsilicons.com`. `backend.remind` is a five-minute CNAME to
the stack's `LoadBalancerDnsName`. Retain the ACM-provided validation CNAME for
automatic certificate renewal. Change only these Remind records; never replace
unrelated records from a stale zone snapshot. Exact assigned endpoints and image
digests are captured in the checked-in production parameters and release record.

## Client and CLI publication

The backend crate has `publish=false` and remains proprietary. Only
`silicon-remind-client` and `silicon-remind-cli` are Apache-2.0 packages.

Run `python3 scripts/sync-package-docs.py` before packaging. Inspect package file
lists and archives for credentials and unrelated server code, build the client
package, then publish it. Wait for the registry to index the client before
packaging and publishing the CLI, which depends on that version. Use the stored
Cargo registry credential without putting it in shell arguments or logs. Record
version numbers before publication: registry versions cannot be overwritten.

Install the released CLI into an isolated Cargo root and verify its help,
version and public health command. For update maintenance, a current-version
result proves registry discovery; replacement by an actually newer version
requires a subsequent release. Library lockfile changes take effect on rebuild.
