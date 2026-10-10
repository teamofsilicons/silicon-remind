#!/usr/bin/env bash
# Independent Accounts services only. Caddy and the IAM service stay untouched.
set -Eeuo pipefail
image=${1:?Pass the digest-pinned Remind backend image}
secret_id=${2:-silicon-remind/accounts-production}
[[ "$image" =~ ^234951665042\.dkr\.ecr\.us-east-1\.amazonaws\.com/silicon-remind-production@sha256:[0-9a-f]{64}$ ]] || exit 2
aws ecr get-login-password --region us-east-1 | docker login --username AWS --password-stdin 234951665042.dkr.ecr.us-east-1.amazonaws.com >/dev/null
docker pull "$image"
umask 077
install -d -m 0700 /etc/remind/accounts
backup=/var/lib/remind/accounts-releases/$(date -u +%Y%m%dT%H%M%SZ)
install -d -m 0700 "$backup"
cp -a /etc/remind/accounts "$backup/config"
aws secretsmanager get-secret-value --region us-east-1 --secret-id "$secret_id" --query SecretString --output text > /etc/remind/accounts/secret.json
python3 - <<'PY'
import json
from pathlib import Path
from urllib.parse import urlparse
base=Path('/etc/remind/accounts');p=base/'secret.json'
try:values=json.loads(p.read_text())
finally:p.unlink()
for k in ['REMIND_APP_SECRET','REMIND_ACCOUNTS_WEBHOOK_SECRET','REMIND_ENCRYPTION_KEYRING','REMIND_DATABASE_URL','REMIND_TEST_DATABASE_URL']:
 if not values.get(k):raise SystemExit('Missing required Accounts runtime key: '+k)
if urlparse(values['REMIND_DATABASE_URL']).path!='/silicon_remind_accounts':raise SystemExit('Refusing legacy production database')
if urlparse(values['REMIND_TEST_DATABASE_URL']).path!='/silicon_remind_accounts_test':raise SystemExit('Refusing legacy testing database')
if any(any(c in str(v) for c in '\r\n\0') for v in values.values()):raise SystemExit('Invalid env value')
p=base/'runtime.env';p.write_text(''.join(k+'='+str(v)+'\n' for k,v in sorted(values.items())));p.chmod(0o600)
print('Independent Accounts env file prepared; secret values omitted.')
PY
for component in api worker; do
  memory=256m; [ "$component" = worker ] && memory=128m
  unit=remind-accounts-$component
  install -d -m 0700 -o 10001 -g 10001 "/var/lib/remind/accounts-telemetry-$component"
  if [ -f "/etc/systemd/system/$unit.service" ]; then cp -p "/etc/systemd/system/$unit.service" "$backup/"; fi
  cat > "/etc/systemd/system/$unit.service" <<UNIT
[Unit]
Description=Remind Accounts $component (isolated from IAM)
Requires=docker.service
After=docker.service network-online.target
Wants=network-online.target
[Service]
Restart=always
RestartSec=5
ExecStartPre=-/usr/bin/docker rm -f $unit
ExecStart=/usr/bin/docker run --name $unit --network remind --entrypoint /usr/local/bin/remind-$component --read-only --cap-drop ALL --security-opt no-new-privileges --tmpfs /tmp:size=32m,mode=1777 --memory $memory --env-file /etc/remind/accounts/runtime.env --mount type=bind,src=/var/lib/remind/accounts-telemetry-$component,dst=/var/lib/remind/telemetry --log-opt max-size=10m --log-opt max-file=3 $image
ExecStop=/usr/bin/docker stop -t 30 $unit
TimeoutStopSec=40
[Install]
WantedBy=multi-user.target
UNIT
done
systemctl daemon-reload
systemctl enable remind-accounts-api remind-accounts-worker
systemctl restart remind-accounts-api remind-accounts-worker
for attempt in {1..30}; do
  if docker exec remind-accounts-api busybox wget -q -O /dev/null http://127.0.0.1:8080/health/ready; then
    echo "Accounts API ready on the private Docker network; legacy services and Caddy unchanged. Backup: $backup"
    exit 0
  fi
  sleep 1
done
echo 'Accounts readiness failed; inspect only the new units. Legacy services remain running.' >&2
exit 1
