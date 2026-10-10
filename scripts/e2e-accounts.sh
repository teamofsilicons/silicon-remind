#!/usr/bin/env bash
# End-to-end checks of Remind against a local Silicon Accounts stack (real sign-ins, tokens, proofs, webhooks):
#
#   REMIND_TEST_STACK=/path/to/test-stack.json scripts/e2e-accounts.sh [--only 1,4,5] [--keep-running]
#
# Starts scripts/dev-accounts.sh when it is not running (and stops it afterwards), makes fresh test accounts,
# and runs the eight scenarios. Details and configuration: python3 scripts/e2e_accounts.py --help.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${PYTHON:-python3}" -I "$here/e2e_accounts.py" "$@"
