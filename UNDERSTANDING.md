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

For each silicon crons can be set, it can be set for one time or recurring. 

The systax for setting a cron would be the same as how cron syntax is in linux. 

For when a cron actually hits, it sends a websocket request via [hook.teamofsilicons.com/silicon/{silicon-id}/] for that particular silicon.

Crons cannot be set for a carbon. Each CRON would have an ID attached to it. And each cron would have a text assigned to it. This text must be sent at the time when cron is hit. 

It should also be possible to specify the timezone in the `tz identifier` format. 


The request for setting a cron would always come from a silicon, and any carbon in the system should be able to view crons of any silicon in their organisation. 

Any silicon should also be able to list their and other silicons in their org crons. 

It should also be possible for a silicon to delete their own crons. 



For one time tasks, it should be possible to set the time using the same cron syntax but only one time. And this would automatically archite it after that. 




For the archived reminds, store them for 45 days and then full archive them.
