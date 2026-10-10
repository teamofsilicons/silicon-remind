# IAM 5 application migration

This branch prepares Remind for IAM 5. Production verification is pending the
coordinated IAM rollout. The backend vendors the reviewed SDK5 candidate at
`f1e9c4768029aacabe337ca41be52e05023d1631`; see vendor provenance.

Login and introspection require one Carbon or Silicon account and one organization.
Legacy unscoped credentials trigger reauthentication. Testing actor-ID login can
select org_id explicitly. Remind has no OBO provider roots or outgoing delegated
features, so ordinary application credentials remain the only Remind login state.

Browser and CLI retain separate contexts per world, account and organization.
Refresh is serialized and preserves the actor/org binding and persisted retry key.
The browser rejects stale account requests and keeps drafts bound to their original
context. CLI --account/--org select saved contexts; auth contexts shows only public
identity metadata. Deleting/forgetting a testing environment removes every saved
context for it.

## Local validation

- 21 Rust client/CLI unit and executable contract tests passed.
- 6 backend IAM security tests and 6 OpenAPI contract tests passed.
- 5 browser gateway/telemetry tests passed, including Carbon and Silicon contexts,
  cross-context writes/logout, restart persistence and changed refresh destination.
- Frontend typecheck/build, strict workspace/all-target Clippy and diff check passed.
- Full backend unit-suite attempt reached Docker-backed database fixtures that
  did not start; it was interrupted. Full database-suite success is not claimed.
- No production migration/deployment or live IAM5 integration claimed.

## Runtime acceptance

After IAM5 is live, sign in with a Carbon and Silicon, add two org/account contexts,
verify parallel tabs reject stale mutations, verify refresh and IAM revocation,
and repeat in isolated testing with cleanup. Publish the compatible app only
through the coordinated release workflow.
