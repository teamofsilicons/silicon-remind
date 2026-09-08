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

Build and push an ARM64 backend image, resolve its digest, then deploy:

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
