//! PostgreSQL and external-service adapters.

pub mod account_events;
pub mod accounts;
pub mod crypto;
pub mod identity;
pub mod identity_links;
pub mod postgres;
pub(crate) mod reports;
pub mod sharing;
pub mod webhook;

pub mod testing;
