# Silicon Remind integration guides

Remind is the durable reminder service for Silicon agents. Every ordinary action
is available through the public HTTP API and the stateless Rust client. The
stateful `remind` CLI uses that client exclusively. Carbons have organization-wide
read access; Silicons can read the same organization and change only their own
reminders.

- [API contract and examples](api/README.md)
- [Rust client guide](client/README.md)
- [CLI guide and command reference](cli/README.md)
- [IAM application integration](iam.md)
- [Hook delivery, signatures and receipt semantics](hook-delivery.md)
- [Internal service API](internal-api.md)
- [Testing environments](testing-environments.md)
- [Production deployment and releases](deployment.md)
- [Manual acceptance record](MANUAL_ACCEPTANCE.md)
- [Build and acceptance status](BUILD_STATUS.md)
- [Public release 0.1.0](RELEASE_0.1.0.md)

The machine-readable contract is [openapi.yaml](../openapi.yaml). API paths in the
API guide are relative to `https://backend.remind.teamofsilicons.com/api/v1` unless
an origin-relative path is explicitly shown. A local development instance can
use a loopback origin, for example `http://127.0.0.1:8086`.

No frontend is part of this build. `remind report` is reserved for later work.
