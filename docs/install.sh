#!/bin/sh
# Install the remind CLI with Silicon Apps, which also keeps it up to date.
# Usage: sh install.sh    (development releases: silicon-apps install 'remind>dev')
set -eu
apps=$(command -v silicon-apps 2>/dev/null || true)
if [ -z "$apps" ] && [ -x "${SILICON_HOME:-$HOME}/.apps/bin/silicon-apps" ]; then
  apps="${SILICON_HOME:-$HOME}/.apps/bin/silicon-apps"
fi
if [ -z "$apps" ]; then
  echo 'remind is installed with Silicon Apps, and silicon-apps was not found.' >&2
  echo 'Install it first: https://developers.teamofsilicons.com/docs/apps/start/install' >&2
  echo 'Then run: silicon-apps install remind' >&2
  exit 1
fi
exec "$apps" install remind
