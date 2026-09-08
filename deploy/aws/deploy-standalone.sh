#!/usr/bin/env bash
# Create/update the single-instance deployment. This script never builds or
# publishes images; pass a digest-pinned ECR URI produced by the release job.
set -Eeuo pipefail
region=${AWS_REGION:-us-east-1}
profile=${AWS_PROFILE:-silicon-production}
stack=${STACK_NAME:-silicon-remind-standalone}
backend_image=${1:?usage: deploy/aws/deploy-standalone.sh <backend-image-uri> [caddy-image-uri]}
caddy_image=${2:-caddy:2.10.2-alpine}

case "$backend_image" in
  *"@sha256:"*) ;;
  *) echo "Backend image must be pinned by @sha256 digest" >&2; exit 2;;
esac

aws --profile "$profile" --region "$region" cloudformation deploy \
  --stack-name "$stack" \
  --template-file "$(dirname "$0")/standalone.yaml" \
  --capabilities CAPABILITY_NAMED_IAM \
  --parameter-overrides BackendImageUri="$backend_image" CaddyImageUri="$caddy_image" \
  --tags Service=silicon-remind Environment=production Deployment=standalone-ec2

aws --profile "$profile" --region "$region" cloudformation describe-stacks \
  --stack-name "$stack" \
  --query 'Stacks[0].Outputs' --output table
