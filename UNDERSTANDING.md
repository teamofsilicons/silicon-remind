# UNDERSTANDING.md - Remind

This is understanding.md for silicon remind. Silicon remind is our reminder system that manages all the cron's for a given silicon and notifies.

# Glossary

`Carbon` - The human in the system. Every human account is called a carbon.
`Silicon` - Our AI Agent (silicon) account is refered to as a Silicon.
`Org` - This is our organisation, this is where all the silicons and carbons would stay for a single organisation and defines the scope.


# How login works

Logging in and signing up are handled entirely by Silicon IAm (this is our access and authorization management layer). You would have an app_id and app_secret stored in your env that you can use to request the login and signup from Silicon IAm (read [(https://github.com/teamofsilicons/silicon-iam/tree/main/docs/client)]) you would realise how you would need to login and singup using silicon IAm. For both signing in and signing up into the system would need Silicon IAm authorization, once you have the access token from SIlicon IAm for the user logged in, render the application accordingly.

Use the oficial and latest silicon client for using IAm at all times and across everywhere. (https://crates.io/crates/silicon-iam-client/)

The webhook endpoint ([backend.remind.teamofsilicons.com/webhook/]) you have would give you information whenever someone logs out, kicked from org, anything changes you would know.


# How it works

For each silicon reminders can be set, it can be set for one time or recurring.

The systax for setting a reminder would be the same as how cron syntax is in linux.
`* * * * *` - `minute, hour, day of month, month, day of week`.

For each silicon that is registered into the system they need to have logged in via IAM. Webhook subscriptions are optional; a Silicon may configure zero, one, or multiple webhook URLs, and each subscribed URL receives reminder events.

Past creation it should be possible to turn off any single or a set of reminders at any time and still keep it active, and can turn it back on anytime needed.

For carbons that log in should see all the silicons they have access to and inside each silicon see all the reminders that silicon set, they wont be able to remove the reminder or perform any action just view.

Each reminder would have an ID attached to it. And each reminder would have a text assigned to it. This text must be sent at the time when reminder time is actually reached and the message should go to every configured webhook subscription.

If no webhook endpoint is configured, reminders can still be created and retained; they simply have no outbound webhook deliveries until subscriptions are added.

### Timezone

For each reminder it should also be possible to specify the timezone in the `tz identifier` format. This is the Timezone that can be used to send the remind at the correct time, this is optional and would by default use the UTC.

Everytime the trigger occurs recalculate the next trigger time in utc for the specific reminder based on the set timezone. And store the said trigger time so it's triggered when the time comes.

---

The request for setting a reminder would always come from a silicon, and any carbon in the organisation should be able to view reminders of any silicon in their organisation.

Any silicon should also be able to list their reminders and other silicons reminders in their org.

It should also be possible for a silicon to archive their reminder. These archived reminders would go in the archived section for 45 days before completely deleting them. Which should also be stored in a list of deleted_reminders log that stores every single reminder that has been deleted past the 45 days window in text format, keep it rolling past 100,000 lines - so store the latest 100,000 deleted reminders.

For each deleted reminder also store what the reminder was when the trigger was, and who set it.


For one time tasks, it should be possible to set the time using the same cron syntax but only one time. And this would automatically archite it after that.



# Testing

We will have an test enviorment for remind itself, this would be an exact replica of the main application, so when the test enviorment is created it would be initiated empty, for the said test enviorment actions can be performed, as this is an exact same replica of the main prod.

Refer to this to know how to create testing enviorment compatible with iam.
https://github.com/teamofsilicons/silicon-iam/blob/main/docs/client/testing-environments.html

For creating a test enviorment on remind, it would require the name of the test enviorment and also the test enviroment key of iam, this test iam key would be used in the each request it sends to the IAm as this is in test enviorment, it would in no way be possible to send request to it without attaching the test enviorment.

So remind testing wouldn't support remind testing on the prod IAm, it would only support it in the testing enviorment of IAm.

Once the name and the test-key to silicon iam is given, the remind would also generate a test key, this test key can be used by any one to perform any action in silicon-remind.

For each testing enviorment they would be sharing a shared test database, this would just be an isolated table in the db storing the linking for all the test enviorments.

A test enviorment is basically the exact same remind with all the functions and everything else, so this is the remind where i can test creating a remind, seeing if i am getting the reminder as expected, basically test it all out.


### Creating Test Env

For creating a test enviorment, it can be created by any carbon or silicon in the organisation and it would be owned by the organisation with the user marked as the creator of the test enviorment. The test enviorment is created at the silicon-remind level itself. For creating a test enviorment it would need the name, an optional description, and the iam test enviorment.

In return it would return the key for the test enviorment, this key is what's gonna be used to be able to access that test enviorment, anyone with this key would be able to access the test enviorment as the god of the test enviorment, this key would be stored along side with the test enviorment, and can anytime be retrieved by the said carbon/silicon/org_admin/org_owner. The key would be 32 digit alpha numeric.

### Rotate Key

The creator of the test enviorment and org_admin/org_head should be able to rotate the key of the test enviroment, which would give them a new key to the test enviorment.

### Clean Test Enviorment

There should be an option to clean the test enviorment, which would allow the test enviorment to be there, but would clear every signle data stored for the said test enviorment. Anyone with the key should be able to execute this action.

### Delete Test Env

The org admins, owners or the creator should be able to delete the test enviorment, deleting a test enviorment would delete the key, and the instance that the test enviorment even existed. For all the logs it should also be limited to the test enviorment itself. Each deleted Test Env would have a ttl of 30 days before getting deleted permanently. From this point the test env should be recoverable.

### Auto Delete Test Env

If there's no new activity in the test enviorment for 15 days, auto delete the test enviorment.

### Using a Test Enviorment

For using a test enviorment anyone with the key would have the god view for that test enviorment, they should be able to access remind as the signed in user from IAm, and now as the signed in user it should be able to perform the set of allowed actions, so this is an exact replica of how remind would have worked with the actual iam, instead it has the test remind and the test iam, so an sandboxed enviorment to test it all out.

A maximum of 100 reminds can be created in test enviorment, be clear to mention this is just a test enviorment limitation.

Read [(https://github.com/teamofsilicons/silicon-iam/blob/main/docs/client/testing-environments.html)] to understand how exactly are webhooks gonna work for this, etc.

---
---
---
---
---
---
---
---
---
---
---
---
---

Only above this line is what the remind backend would hold, below this would be the users of the backend, the client, the frontend, the cli, etc.

# Rust Package & CLI

The Rust package & cli using that rust package are first hand client with an always running deamon if needed in the background. the UI will be a subset of the cli. make sure everything works via the CLI first, and then we'll make the UI. Everyone should be able to use the CLI/Rust Package (carbons, silicons, org, access keys, api keys, read, write, patch, delete, everything).

The rust package would be stateless whereas the cli would be statefull. CLI built on top of the rust package.

For how this CLI is built, rust as the programming language, but can use anything under the hood that is needed. Maybe rust, or node, or shell, as and when the work comes. That is decided by the implementor based on the work. If something requirs a UI (like graph, live, video, images etc). for that the UI has an endpoint that can be viewed/used/downloaded and the cli gives the link to that.

The primary Interface is the Rust Package. CLI is built using the Rust Package only and doesn't have any feature that the Rust package does not.

if you need a local store for auth or something else, use `{home_dir}/.{appname}/dir`.

The default home dir is `~`.

For both package and the cli write detailed docs on how to use the package and how to use the cli, and also another doc on how to use the package.

Package and CLI must only expose the client side actions, and not the internal actions performed by the backend. For the CLI follow the standard command line grammar rules, and also include a -h command that shows all the possible commands.

Testing in the test enviorment should also be possible via both cli, and the package.

Testing enviorment in cli, for testing enviorment in cli i should just be able to `remind --test <test_id> <command>` infront of the same command and it should treat that as a test command. Same for test only commands even they would have the same style just without specifying --test for them would return this action is only possible for test enviorment.

--- logging in via cli ---

For logging in via the cli or the package for any carbon/silicon you don't ask for their credentials or redirect them anywhere, instead you just request for their short lived token. This short lived token would then be used for the same login logic, the short lived token would be compared and you will get the refresh and auth token.

For CLI login there should be this exact command: `remind login <slt>`.
And there should be an command to configure the home directory where the information is stored:  `{home_dir}/.{appname}/dir`. This can be confitgure via `remind config home {location}`. If it's not a directory give an error not a directory.

For both cli and client we would also package in an auto updater, the task of this auto updater is to compare the current version to the latest version in crates for them, and if there's a new verion auto update it to the said new version. By default auto update is on, users can specifically come and opt in to stop auto update. Which would stop auto updating the package. Auto updater check runs every single hour. Updates should be checked when the command is run and should happen every hour, so check for the last update check time and if it's past 1 hour old check for update and update after the command finishes running.

### Cli experience

Cli is an interface on it's own, it's an interface used by our fellow dear agents, and sometimes humans. What we would want this interface to serve as is it should give the correct information at correct time, and can write texts to explain what exactly is happening.

A few things that would be needed to ensure good cli experience: the cli alone should have enough information to use remind correctly! Surfacing the right set of things when needed, giving suggestions at the correct times. Like for eg: when someone runs a command then show them the exact help for it if the information is not enough, and when the app has been created, show them the other related commands that they might need to run after it. For each command a good description, the entire docs, etc.

So the overall cli experience needs to be super good. It needs to give the relevant informations, help should be detailed, and suggested commands, etc should also happen.

# Docs

The API, Rust-client, CLI, IAM integration, and testing-environment guides are
maintained in [docs/].

For the docs keep it as detailed and mention all the details, this is the only thing the other apps can use as their source of knowledge and how they can use remind exactly.

Write detailed guides.

Write very good detailed instructions on how test enviorment for silicon-remind works. Write docs on all 3 cli, api, client. Keep it segregated and clear. Write all the documentations in docs/ folder in the main directory of silicon-remind.

# Later to do

remind report `<report-message>`, this should send an report message to the user.
