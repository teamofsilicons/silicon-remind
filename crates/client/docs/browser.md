# Use Remind in your browser

Open [Remind](https://remind.teamofsilicons.com) and choose **Continue with IAM**. IAM handles credentials and consent; Remind receives a short-lived token and stores the resulting session encrypted on its server. Select one of the organizations you authorized.

Use Reminders to create or edit schedules as a Silicon, pause/resume selected reminders, or archive one. Carbons can inspect organization reminders and execution history. Archived items remain readable for 45 days. Webhooks are optional destinations for the signed-in Silicon.

When creating a reminder, enter an IANA identifier such as `Asia/Kolkata` or `UTC`
in the **Timezone** field. A timezone is mandatory; leaving it blank returns an
error, and Remind does not select a default.

## Test safely

Choose **Use a test environment** from sign-in or settings and paste the IAM test application's `app_secret`. Its name and environment are discovered for you. Sign in using a test SLT or an existing active test Carbon/Silicon's public ID. Unknown or inactive identities fail. The top banner shows the environment name and identity; **Exit testing mode** returns to your production session. Each environment has separate tokens and selection state.

An invalid secret never selects production. Normal sandbox use applies the signed-in user's real permissions. IAM is the administrative surface for cleaning or retiring discovered worlds; the legacy Remind administrative controls are separate. [Full testing guide](testing-environments.md).

## Configuration and troubleshooting

Settings exposes organization and session information and the sandbox selector. The CLI exposes the complete configuration surface, offline help, service management, and developer workflows. If a request fails, retain its request ID for diagnosis and recheck the selected environment and identity before retrying. A network or authorization error does not switch environments.

## Telemetry preference

Settings → Telemetry controls operational analytics and event collection for this browser. It is on by default. Turning it off clears pending browser events and sends the opt-out header on application requests. The official Space Station web package collects automatic interactions and timing; Remind reduces these to fixed event codes, durations, and status codes before sending them. No URLs, error messages, input contents, tokens or arbitrary browser metadata are forwarded. Sessions changing environments cancel the old analytics sender; the gateway rejects batches for a different active environment.
