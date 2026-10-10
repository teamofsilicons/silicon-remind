# Internal API documentation

Remind has no internal provisioning API. The routes that only Silicon Accounts and the operators call (the app
webhook, health and metrics) and the operator commands are described in
[docs/internal-api.md](docs/internal-api.md). Everything a Carbon, a Silicon or another app calls is in the
[public API](docs/api/README.md).
