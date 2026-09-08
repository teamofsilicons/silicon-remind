# IAM integration

Remind is the organization-qualified IAM Application `tos>remind`. Application
credentials stay in backend environment variables:

- `REMIND_IAM_BASE_URL`: IAM service origin.
- `REMIND_IAM_APP_ID`: canonical Application ID.
- `REMIND_IAM_APP_SECRET`: Application secret supplied by IAM.
- `REMIND_IAM_WEBHOOK_KEYRING`: retained signing secrets keyed by IAM key version.

The receiver URL is `https://backend.remind.teamofsilicons.com/webhook/`. Register
that exact URL in IAM. The existing internal receiver alias remains available
for retained integrations, but new registration should use `/webhook/`.

All IAM HTTP operations use the published `silicon-iam-client` crate. The backend
disables runtime dependency updates because deployment owns its compiled binary.
Client/CLI users of Remind have separate default-on updater behavior.

## Login and authorization

The CLI and Rust client submit an IAM short-lived token to Remind's login route.
The server exchanges it through `oauth().login` with its Application credential.
Refresh uses `oauth().refresh`; logout uses `oauth().revoke`. No IAM password or
OTP is accepted by Remind. Browser sign-in starts an unscoped IAM login: the user
selects the organizations to authorize in IAM itself. Remind exchanges the SLT
without an organization header, then discovers the granted organizations through
`GET /api/v1/auth/organizations`. The sidebar switches between those grants.
`X-Org-ID` selects an already-authorized organization for reminder requests; it
does not add grants or scope a new login.

Every authenticated request calls IAM introspection with the requested org and
requires an active Application access token, future expiry, matching app/client
audience, organization, principal, membership, and testing-environment identity.
Its current `ApplicationAuthorization` snapshot supplies the public identity,
org role and authorization epoch. An undisclosed org role never becomes admin.
Carbons and Silicons receive organization-wide reminder reads; only the owning
Silicon receives reminder mutation authority.

The synchronous snapshot also binds IAM's immutable organization UUID to its
public org handle, allowing signed lifecycle events to find the correct org
without trusting an arbitrary routing string. Remind does not wait for a
webhook before an initial authenticated request can obtain its permissions.

## Signed events

The official verifier authenticates the raw body before JSON normalization. It
checks event ID agreement, unique security headers, signature, signing version,
body bounds, and timestamp replay tolerance. Retain old key versions while
in-flight events can still arrive. Never decode or alter the signing secret's
literal bytes before verification.

Current IAM projection envelopes put member state under `data.current.members`
and organization state under `data.current.organization`. Remind consumes
revocations and records other supported additive events durably. A receipt and
all its projected revocations commit atomically. The older minimal projection is
accepted for compatible retained deliveries. Live introspection makes an expired
or revoked caller token unusable even before asynchronous events arrive.

For sandbox setup, test Application credentials and wrapped events, see
[testing environments](testing-environments.md) and the official
[IAM client manual](https://github.com/teamofsilicons/silicon-iam/tree/main/docs/client).

## Configuration still requires deployment proof

Registering an app or storing credentials does not deploy the Remind receiver.
Verify IAM's actual assigned key version, the public route's TLS/reachability,
required webhook scopes, and delivery to the deployed receiver before treating
the production integration as enabled. The local manual run log distinguishes
local runtime proof from a deployed endpoint.
