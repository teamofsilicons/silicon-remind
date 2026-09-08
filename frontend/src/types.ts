export interface Identity {
  principal_id: string;
  public_id: string;
  actor_type: string;
  org_id: string;
  org_role: string;
  can_manage_reminders: boolean;
}
export interface Environment {
  id: string;
  name: string;
  description: string | null;
  org_id: string;
  creator_id: string;
  iam_environment_id: string;
  deleted_at: string | null;
  purge_after: string | null;
  created_at: string;
  last_activity_at: string;
  version: number;
}
export interface Context {
  id: string;
  name: string;
  org: string;
  organizations: string[];
  identity: Identity | null;
}
export interface Session {
  active: string;
  contexts: Context[];
  identity: Identity | null;
  productionIdentity: Identity | null;
}
export interface Schedule {
  id: string;
  text: string;
  cron: string;
  timezone: string;
  kind: string;
  status: string;
  section: string;
  silicon_id: string;
  org_id: string;
  next_run_at: string | null;
  archived_at: string | null;
  purge_after: string | null;
  created_at: string;
  updated_at: string;
}
export interface Execution {
  id: string;
  scheduled_for: string;
  attempted_at: string | null;
  delivered_at: string | null;
  status: string;
  failure_reason: string | null;
  hook_event_id: string | null;
}
export interface Silicon {
  principal_id: string;
  silicon_id: string;
  reminder_count: number;
}
export interface Page<T> {
  items: T[];
  next_cursor: string | null;
}
