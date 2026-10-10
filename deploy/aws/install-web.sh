#!/usr/bin/env bash
# Run through SSM as root on the standalone Remind instance (deploy/aws/deploy-web.py sends it).
#
# Installs the Next.js web for https://remind.teamofsilicons.com from a digest-pinned image built from frontend/
# (Next.js standalone server: WORKDIR /app, listens on $PORT, runs as a non-root user). The web signs Carbons in with
# Silicon Accounts and keeps each session in a sealed httpOnly cookie, so it needs no session storage; it calls the
# Remind API over the private `remind` Docker network.
#
# Settings live in /etc/remind/web.env (root, 0600): APP_SECRET is read from the runtime secret's REMIND_APP_SECRET on
# every run, SESSION_SECRET is generated once and kept (rotating it signs every browser out), and any other value an
# operator changed in the file is kept. The SolidJS frontend this replaces (remind-frontend.service) is stopped and
# disabled; its /etc/remind/frontend.env and /var/lib/remind-frontend are left untouched.
set -Eeuo pipefail
image=${1:?Pass the digest-pinned web image URI}
secret_arn=${2:-arn:aws:secretsmanager:us-east-1:234951665042:secret:silicon-remind/runtime-production-dbqkfb}
case "$image" in
  234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-remind-production@sha256:*) ;;
  *) echo 'Expected a digest-pinned Remind ECR image' >&2; exit 2;;
esac
aws ecr get-login-password --region us-east-1 | docker login --username AWS --password-stdin 234951665042.dkr.ecr.us-east-1.amazonaws.com >/dev/null
docker pull "$image"
install -d -m 0700 /etc/remind
(umask 077 && aws secretsmanager get-secret-value --region us-east-1 --secret-id "$secret_arn" \
  --query SecretString --output text > /etc/remind/web-secret.json)
python3 - <<'PY'
import json, os, secrets
from pathlib import Path
source_path = Path('/etc/remind/web-secret.json')
try:
    source = json.loads(source_path.read_text())
finally:
    source_path.unlink()
app_secret = source.get('REMIND_APP_SECRET')
if not isinstance(app_secret, str) or not app_secret or any(c in app_secret for c in '\r\n\0'):
    raise SystemExit('The runtime secret has no REMIND_APP_SECRET; add Remind\'s Silicon Accounts app secret first.')
path = Path('/etc/remind/web.env')
kept = {}
if path.exists():
    for line in path.read_text().splitlines():
        key, separator, value = line.partition('=')
        if separator:
            kept[key] = value
values = {
    'APP_ID': 'remind',
    'ACCOUNTS_URL': source.get('ACCOUNTS_URL') or 'https://accounts.teamofsilicons.com',
    # The web calls the contract-2 paths of openapi.yaml (/schedules, /silicons, /auth/me, ...).
    'APP_API_URL': 'http://remind-api:8080/api/v2',
    'PUBLIC_URL': 'https://remind.teamofsilicons.com',
    'PORT': '3000',
    'HOSTNAME': '0.0.0.0',
    'NODE_ENV': 'production',
}
values.update({key: value for key, value in kept.items() if key not in ('APP_SECRET', 'SESSION_SECRET')})
values['APP_SECRET'] = app_secret
values['SESSION_SECRET'] = kept.get('SESSION_SECRET') or secrets.token_urlsafe(48)
with open(path, 'w', opener=lambda p, f: os.open(p, f, 0o600)) as output:
    for key, value in sorted(values.items()):
        output.write(key + '=' + value + '\n')
os.chmod(path, 0o600)
print('Wrote /etc/remind/web.env:', ', '.join(sorted(values)))
PY
cat > /etc/systemd/system/remind-web.service <<UNIT
[Unit]
Description=Silicon Remind web (Next.js, signs in with Silicon Accounts)
Requires=docker.service
After=docker.service remind.service network-online.target
Wants=network-online.target
[Service]
Type=simple
Restart=always
RestartSec=5
ExecStartPre=-/usr/bin/docker rm -f remind-web
ExecStart=/usr/bin/docker run --name remind-web --network remind --env-file /etc/remind/web.env --read-only --tmpfs /tmp:rw,noexec,nosuid,size=32m --tmpfs /app/.next/cache:rw,noexec,nosuid,size=64m --cap-drop ALL --security-opt no-new-privileges:true --memory 384m --log-opt max-size=10m --log-opt max-file=3 $image
ExecStop=/usr/bin/docker stop -t 20 remind-web
TimeoutStopSec=30
[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload
systemctl enable remind-web.service
systemctl restart remind-web.service
for attempt in {1..30}; do
  if docker exec remind-web node -e 'fetch("http://127.0.0.1:3000/").then(r=>process.exit(r.status<500?0:1)).catch(()=>process.exit(1))'; then break; fi
  if [ "$attempt" = 30 ]; then
    echo 'The web did not answer on port 3000; read journalctl -u remind-web (a missing setting is named there).' >&2
    exit 1
  fi
  sleep 1
done
# The SolidJS frontend and its session gateway are replaced; keep their files for the record.
if [ -f /etc/systemd/system/remind-frontend.service ]; then
  systemctl disable --now remind-frontend.service || true
fi
# Point the remind.teamofsilicons.com vhost at the web, keeping every other route and the Caddyfile inode
# (it is bind-mounted into the Caddy container).
cp -p /etc/remind/Caddyfile /etc/remind/Caddyfile.before-web
python3 - <<'PY'
from pathlib import Path
path = Path('/etc/remind/Caddyfile')
lines = path.read_text().splitlines(keepends=True)
block = ['remind.teamofsilicons.com {\n', '  encode gzip\n', '  reverse_proxy remind-web:3000\n', '}\n']
start = next((i for i, line in enumerate(lines) if line.strip() == 'remind.teamofsilicons.com {'), None)
if start is None:
    lines += ['\n'] + block
else:
    end = next(i for i in range(start + 1, len(lines)) if lines[i].rstrip('\n') == '}')
    lines[start:end + 1] = block
with open(path, 'r+') as caddyfile:
    caddyfile.seek(0)
    caddyfile.write(''.join(lines))
    caddyfile.truncate()
PY
if ! docker exec remind-caddy caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile; then
  cat /etc/remind/Caddyfile.before-web > /etc/remind/Caddyfile
  exit 1
fi
docker exec remind-caddy caddy reload --config /etc/caddy/Caddyfile --adapter caddyfile
echo 'Web installed: remind-web.service serves https://remind.teamofsilicons.com; verify it over public HTTPS.'
