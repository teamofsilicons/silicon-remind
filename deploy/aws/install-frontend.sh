#!/usr/bin/env bash
# Run through SSM as root on the standalone Remind instance.
set -Eeuo pipefail
image=${1:?Pass the digest-pinned frontend image URI}
case "$image" in
  234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-remind-production@sha256:*) ;;
  *) echo 'Expected a digest-pinned Remind ECR image' >&2; exit 2;;
esac
aws ecr get-login-password --region us-east-1 | docker login --username AWS --password-stdin 234951665042.dkr.ecr.us-east-1.amazonaws.com >/dev/null
docker pull "$image"
install -d -m 0700 /etc/remind
install -d -m 0700 -o 10001 -g 10001 /var/lib/remind-frontend/sessions
python3 - <<'PY'
import os, secrets
path = '/etc/remind/frontend.env'
if not os.path.exists(path):
    with open(path, 'x', opener=lambda p, f: os.open(p, f, 0o600)) as f:
        f.write('SESSION_KEY=' + secrets.token_urlsafe(32) + '\n')
        f.write('SESSION_DIRECTORY=/sessions\n')
        f.write('FRONTEND_ORIGIN=https://remind.teamofsilicons.com\n')
        f.write('REMIND_UPSTREAM=https://backend.remind.teamofsilicons.com\n')
        f.write('IAM_AUTH_ORIGIN=https://auth.iam.teamofsilicons.com\n')
os.chmod(path, 0o600)
PY
cat > /etc/systemd/system/remind-frontend.service <<UNIT
[Unit]
Description=Silicon Remind SolidJS frontend and session gateway
Requires=docker.service
After=docker.service remind.service network-online.target
Wants=network-online.target
[Service]
Type=simple
Restart=always
RestartSec=5
ExecStartPre=-/usr/bin/docker rm -f remind-frontend
ExecStart=/usr/bin/docker run --name remind-frontend --network remind --env-file /etc/remind/frontend.env --mount type=bind,source=/var/lib/remind-frontend/sessions,target=/sessions --read-only --tmpfs /tmp:rw,noexec,nosuid,size=32m --cap-drop ALL --security-opt no-new-privileges:true --memory 256m --log-opt max-size=10m --log-opt max-file=3 $image
ExecStop=/usr/bin/docker stop -t 20 remind-frontend
TimeoutStopSec=30
[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload
systemctl enable remind-frontend.service
systemctl restart remind-frontend.service
for attempt in {1..30}; do
  if docker exec remind-frontend node -e 'fetch("http://127.0.0.1:4330/").then(r=>process.exit(r.status===200?0:1)).catch(()=>process.exit(1))'; then break; fi
  if [ "$attempt" = 30 ]; then exit 1; fi
  sleep 1
done
# Preserve the backend routes and Caddy file inode (it is bind-mounted).
cp -p /etc/remind/Caddyfile /etc/remind/Caddyfile.before-frontend
python3 - <<'PY'
from pathlib import Path
p = Path('/etc/remind/Caddyfile')
s = p.read_text()
if 'remind.teamofsilicons.com {' not in s.split('backend.remind.teamofsilicons.com {', 1)[-1]:
    s += '\nremind.teamofsilicons.com {\n  encode gzip\n  reverse_proxy remind-frontend:4330\n}\n'
    p.write_text(s)
PY
if ! docker exec remind-caddy caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile; then
  cat /etc/remind/Caddyfile.before-frontend > /etc/remind/Caddyfile
  exit 1
fi
docker exec remind-caddy caddy reload --config /etc/caddy/Caddyfile --adapter caddyfile
echo 'Frontend installed; configure Namecheap remind DNS and verify public HTTPS.'
