# UNDERSTANDING.md - Remind

This is understanding.md for silicon remind. Silicon remind is our reminder system that manages all the cron's for a given silicon and notifies. 

# Glossary

`Carbon` - The human in the system. Every human account is called a carbon.
`Silicon` - Our AI Agent (silicon) account is refered to as a Silicon.
`Org` - This is our organisation, this is where all the silicons and carbons would stay for a single organisation and defines the scope. 


# How login works

Logging in and signing up are handled entirely by Silicon IAm (this is our access and authorization management layer). You would have an app_id and app_secret stored in your env that you can use to request the login and signup from Silicon IAm (read [[../silicon-iam/UNDERSTANDING.md]]) you would realise how you would need to login and singup using silicon IAm. For both signing in and signing up into the system would need Silicon IAm authorization, once you have the access token from SIlicon IAm for the user logged in, render the application accordingly. 

The webhook endpoint you have would give you information whenever someone logs out, kicked from org, anything changes you would know.


# How it works

For each silicon reminders can be set, it can be set for one time or recurring. 

The systax for setting a reminder would be the same as how cron syntax is in linux. 
`* * * * *` - `minute, hour, day of month, month, day of week`.

For each silicon that is registered into the system they need to have logged in via IAm, and must have a webhook endpoint configured. This webhook endpoint is the place where you send them all the requests. 

For carbons that log in should see all the silicons they have access to and inside each silicon see all the reminders that silicon set, they wont be able to remove the reminder or perform any action just view.

Each reminder would have an ID attached to it. And each reminder would have a text assigned to it. This text must be sent at the time when reminder time is actually reached and the message should go via the configured webhook endpoint. 

If no webhook endpoint is configured when trying to set a new reminder, it should return an error `Set the webhook url first.`.

### Timezone

For each reminder it should also be possible to specify the timezone in the `tz identifier` format. This is the Timezone that can be used to send the remind at the correct time, this is optional and would by default use the UTC. 

Everytime the trigger occurs recalculate the next trigger time in utc for the specific reminder based on the set timezone. And store the said trigger time so it's triggered when the time comes. 

---

The request for setting a reminder would always come from a silicon, and any carbon in the organisation should be able to view reminders of any silicon in their organisation. 

Any silicon should also be able to list their reminders and other silicons reminders in their org. 

It should also be possible for a silicon to archive their reminder. These archived reminders would go in the archived section for 45 days before completely deleting them. Which should also be stored in a list of deleted_reminders log that stores every single reminder that has been deleted past the 45 days window in text format, keep it rolling past 100,000 lines - so store the latest 100,000 deleted reminders. 

For each deleted reminder also store what the reminder was when the trigger was, and who set it. 


For one time tasks, it should be possible to set the time using the same cron syntax but only one time. And this would automatically archite it after that. 
