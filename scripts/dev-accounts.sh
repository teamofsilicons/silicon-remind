#!/usr/bin/env bash
# Start Remind on this machine against a local Silicon Accounts stack (idempotent):
#
#   REMIND_TEST_STACK=/path/to/test-stack.json scripts/dev-accounts.sh [--build] [--check-webhook]
#
# Creates and migrates the databases remind_e2e and remind_e2e_testing on 127.0.0.1:5460, starts a delivery
# receiver (127.0.0.1:4183), remind-api (127.0.0.1:4181) and remind-worker (127.0.0.1:4182), and points Remind's
# app webhook at Silicon Accounts to http://127.0.0.1:4181/webhook/, proving it with a test ping.
# Stop with scripts/dev-accounts-stop.sh. Configuration and details: python3 scripts/dev_accounts.py --help.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${PYTHON:-python3}" -I "$here/dev_accounts.py" up "$@"
