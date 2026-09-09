# Standalone EC2 deployment

`standalone.yaml` runs the production API and worker on one ARM64 `t4g.small` in
`vpc-04b23a487cfe0bd8e`, public subnet `subnet-07945746462c26b2d`. It does not
create or attach an Application Load Balancer. The instance receives a public
IPv4 address, and Caddy terminates TLS for `backend.remind.teamofsilicons.com`.

The instance has no SSH key or public application ports. Use SSM Session Manager.
Its role can read only the configured runtime secret and pull only the
`silicon-remind-production` ECR repository. RDS access is granted to the
existing security group `sg-0fbecb17f3e0c2521` on port 5432 from the new instance
security group. The runtime secret must contain these non-empty values:
`REMIND_IAM_APP_SECRET`, `REMIND_IAM_WEBHOOK_KEYRING`,
`REMIND_ENCRYPTION_KEYRING`, `REMIND_INTERNAL_API_TOKEN`,
`REMIND_DATABASE_URL`, and `REMIND_TEST_DATABASE_URL`.

Build the ARM64 base image, then wrap it with `deploy/aws/Dockerfile.runtime`
before pushing. The wrapper installs the AWS RDS CA bundle required by the
production database URLs; the base image alone cannot connect to RDS.

```bash
docker build --platform linux/arm64 -t silicon-remind:release .
docker build --platform linux/arm64 -f deploy/aws/Dockerfile.runtime \
  --build-arg BACKEND_IMAGE=silicon-remind:release \
  -t <ecr-repository>:<release-tag> .
docker push <ecr-repository>:<release-tag>
```

Resolve the wrapped image's digest, verify API startup and `/health/ready` in a
temporary container using the existing runtime configuration, then deploy:

```bash
AWS_PROFILE=silicon-production AWS_REGION=us-east-1 \
  deploy/aws/deploy-standalone.sh \
  234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-remind-production@sha256:<backend-digest>
```

The script requires a digest-pinned backend URI. It creates/updates the stack
`silicon-remind-standalone`, validates the parameter defaults, and prints the
instance ID, public IP, and public URL. Point the Namecheap `backend.remind`
record at the output `PublicIp`; Caddy obtains and renews the certificate once
DNS resolves. Port 80 must remain reachable for ACME HTTP fallback.

The template intentionally does not run migrations: schema changes should be
performed by the reviewed migration release procedure against the existing RDS
before switching the image. After deployment, verify through SSM and the public
health endpoint:

```bash
aws --profile silicon-production --region us-east-1 ssm start-session \
  --target <InstanceId>
curl --fail --show-error https://backend.remind.teamofsilicons.com/health/ready
```

Container logs are available in SSM with `docker logs remind-api`,
`docker logs remind-worker`, and `docker logs remind-caddy`.

## Frontend

The SolidJS frontend and Node session gateway run on the same instance, with
Caddy serving `https://remind.teamofsilicons.com`. No load balancer or public
container port is added. Namecheap's `remind` CNAME points to
`backend.remind.teamofsilicons.com`, so both names follow the backend A record.
The instance currently has an automatically assigned public IP: after a
stop/start or replacement, update the backend A record to the new IP.

Build `frontend/Dockerfile` with the `frontend/` build context for `linux/arm64`,
push it to `silicon-remind-production` ECR, then install using its resolved digest:

```bash
AWS_PROFILE=silicon-production AWS_REGION=us-east-1 \
  python3 deploy/aws/deploy-frontend.py \
  234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-remind-production@sha256:<frontend-digest> \
  --instance <InstanceId>
```

The SSM installer creates and enables `remind-frontend.service`, preserves the
existing backend Caddy routes, validates the new configuration and reloads
Caddy. Updates briefly restart only the frontend. To roll back, run the same
command with the previous frontend digest. The installer preserves the session
key in `/etc/remind/frontend.env` (root, mode 0600) and encrypted session files
in `/var/lib/remind-frontend/sessions` (UID/GID 10001, mode 0700). Back up both
together through a protected process before replacing the instance.

The frontend container has a read-only root filesystem, no Linux capabilities,
256 MiB memory limit and rotating logs. Callback query strings are not access
logged. Read logs with `journalctl -u remind-frontend` or `docker logs remind-frontend`.
The frontend installation is separate from CloudFormation: rerun it after
provisioning/replacing the backend host or rerunning its bootstrap.

Verify the public page and gateway after deployment:

```bash
curl --fail --show-error https://remind.teamofsilicons.com/
curl --fail --show-error https://remind.teamofsilicons.com/ui/api/health/ready
```

See [the initial frontend deployment record](frontend-2026-09-08.md).

Current backend release: [0.1.2 IAM discovery and login status](../../docs/RELEASE_0.1.2.md).
Current frontend release: [unscoped IAM sign-in](unscoped-login-2026-09-08.md).
