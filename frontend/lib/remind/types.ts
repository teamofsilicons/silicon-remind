export type Account = {
  uuid: string;
  id: string;
  kind: "carbon" | "silicon" | null;
};
export type Identity = Account & {
  display_name: string;
  can_manage_reminders: boolean;
  visible_silicons: number;
  custodian: Account | null;
};
export type Schedule = {
  id: string;
  owner: Account | null;
  silicon_id: string;
  text: string;
  kind: "one_time" | "recurring";
  cron: string;
  timezone: string;
  status: "active" | "paused" | "completed";
  section: "current" | "archived";
  next_run_at: string | null;
  archived_at: string | null;
  purge_after: string | null;
  created_at: string;
  updated_at: string;
};
export type PageResult<T> = { items: T[]; next_cursor?: string | null };
export type Silicon = {
  uuid: string;
  silicon_id: string;
  display_name: string;
  relation: "self" | "custodian" | "sibling" | "shared";
  reminder_count: number;
};
export type Execution = {
  id: string;
  scheduled_for: string;
  status: string;
  attempts: number;
  failure_reason?: string | null;
  delivered_at?: string | null;
  created_at: string;
};
export type Environment = {
  id: string;
  owner_uuid: string | null;
  owner: Account | null;
  name: string;
  description: string | null;
  deleted_at: string | null;
  purge_after: string | null;
  created_at: string;
  last_activity_at: string;
  version: number;
};
export type Grant = {
  id: string;
  owner: Account;
  viewer: Account;
  created_at: string;
};
export type Allowance = {
  id: string;
  silicon: Account;
  allowed: Account;
  created_at: string;
};
export type Subscription = {
  id: string;
  owner: Account | null;
  silicon_id: string;
  endpoint_url: string;
  updated_at: string;
};
export const date = (value: string | null | undefined) =>
  value
    ? new Date(value).toLocaleString(undefined, {
        dateStyle: "medium",
        timeStyle: "short",
      })
    : "—";
export const accountName = (account: Account | null | undefined) =>
  account?.id || account?.uuid || "Unavailable account";
