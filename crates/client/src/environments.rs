//! Remind's own test environments: isolated copies of Remind with their own data.
//!
//! Manage them from production (no test key): create, list, read, key, rotate, retire,
//! restore. Use one by attaching its key with [`Client::with_test_environment`]; inside it you
//! are still the account you signed in as, and every ordinary method works unchanged.
use crate::{Client, Result, models};
use reqwest::Method;
use uuid::Uuid;

impl Client {
    /// `POST /test-environments`: an empty environment owned by the signed-in account. The
    /// response carries its key (also readable later with [`Client::environment_key`]).
    pub async fn create_environment(
        &self,
        input: &models::CreateEnvironment,
    ) -> Result<models::EnvironmentCreated> {
        self.require_production()?;
        self.json(
            self.request(Method::POST, "/test-environments")?
                .json(input),
        )
        .await
    }

    /// `GET /test-environments`: yours, those of the Silicons you look after, of your
    /// custodian and of its other Silicons, oldest first; `after` is the previous page's
    /// `next_cursor`.
    pub async fn environments(
        &self,
        include_deleted: bool,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<models::Page<models::TestEnvironment>> {
        self.require_production()?;
        let mut query = vec![
            ("include_deleted", include_deleted.to_string()),
            ("limit", limit.to_string()),
        ];
        if let Some(after) = after {
            query.push(("after", after.to_string()));
        }
        self.json(
            self.request(Method::GET, "/test-environments")?
                .query(&query),
        )
        .await
    }

    /// `GET /test-environments/{id}`.
    pub async fn environment(&self, id: Uuid) -> Result<models::TestEnvironment> {
        self.require_production()?;
        self.json(self.request(Method::GET, &format!("/test-environments/{id}"))?)
            .await
    }

    /// `GET /test-environments/{id}/key`: the active key.
    pub async fn environment_key(&self, id: Uuid) -> Result<models::EnvironmentKey> {
        self.require_production()?;
        self.json(self.request(Method::GET, &format!("/test-environments/{id}/key"))?)
            .await
    }

    /// `POST /test-environments/{id}/key-rotations`: a new key; the old one stops working.
    /// Only the owner, or the owner's custodian when the owner is a Silicon.
    pub async fn rotate_environment_key(&self, id: Uuid) -> Result<models::EnvironmentKey> {
        self.require_production()?;
        let path = format!("/test-environments/{id}/key-rotations");
        self.json(self.request(Method::POST, &path)?).await
    }

    /// `POST /test-environments/{id}/restorations`: bring a retired environment back (within
    /// 30 days) with a new key.
    pub async fn restore_environment(&self, id: Uuid) -> Result<models::EnvironmentKey> {
        self.require_production()?;
        let path = format!("/test-environments/{id}/restorations");
        self.json(self.request(Method::POST, &path)?).await
    }

    /// `DELETE /test-environments/{id}`: retire it; recoverable for 30 days.
    pub async fn delete_environment(&self, id: Uuid) -> Result<()> {
        self.require_production()?;
        self.empty(self.request(Method::DELETE, &format!("/test-environments/{id}"))?)
            .await
    }

    /// `GET /testing-environment`: the selected environment, by key alone.
    pub async fn current_environment(&self) -> Result<models::TestEnvironment> {
        self.require_test()?;
        self.json(self.request(Method::GET, "/testing-environment")?)
            .await
    }

    /// `POST /testing-environment/cleanings`: erase every reminder, delivery, subscription and
    /// log of the selected environment, keeping the environment and its key.
    pub async fn clean_environment(&self) -> Result<()> {
        self.require_test()?;
        self.empty(self.request(Method::POST, "/testing-environment/cleanings")?)
            .await
    }
}
