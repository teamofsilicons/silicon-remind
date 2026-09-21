//! Canonical IAM identities mapped to stable, Remind-owned storage keys.

use sqlx::PgPool;
use uuid::Uuid;

/// Resolves only a previously verified public identity inside the selected
/// production or sandbox pool. The database rejects incomplete legacy backfills.
pub(crate) async fn resolve(
    pool: &PgPool,
    kind: &str,
    public_id: &str,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar("SELECT resolve_iam_identity_key($1,$2,$3)")
        .bind(kind)
        .bind(public_id)
        .bind(Uuid::now_v7())
        .fetch_one(pool)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;
    use testcontainers::{ImageExt as _, runners::AsyncRunner as _};
    use testcontainers_modules::postgres::Postgres;

    static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

    #[tokio::test]
    async fn legacy_keys_are_preserved_and_new_bindings_are_isolated() -> anyhow::Result<()> {
        let container = Postgres::default().with_tag("17-alpine").start().await?;
        let url = format!(
            "postgres://postgres:postgres@{}:{}/postgres",
            container.get_host().await?,
            container.get_host_port_ipv4(5432).await?
        );
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await?;
        for migration in MIGRATOR.iter().filter(|migration| migration.version < 8) {
            sqlx::raw_sql(migration.sql.clone()).execute(&pool).await?;
        }
        let old_silicon = Uuid::now_v7();
        let old_carbon = Uuid::now_v7();
        sqlx::query("INSERT INTO organization_lifecycle(org_id,state) VALUES('alpha','active')")
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO silicon_identities(org_id,principal_id,silicon_id,state) VALUES('alpha',$1,'agent:alpha','active')")
            .bind(old_silicon).execute(&pool).await?;
        sqlx::query("INSERT INTO audit_records(id,actor_type,actor_id,action,resource_type) VALUES($1,'carbon',$2,'test','identity')")
            .bind(Uuid::now_v7()).bind(old_carbon.to_string()).execute(&pool).await?;
        sqlx::raw_sql(include_str!(
            "../../migrations/0008_canonical_iam_identity_bindings.sql"
        ))
        .execute(&pool)
        .await?;
        assert_eq!(resolve(&pool, "silicon", "agent:alpha").await?, old_silicon);
        assert!(resolve(&pool, "carbon", "person").await.is_err());
        sqlx::query("INSERT INTO iam_identity_bindings VALUES('carbon','person',$1)")
            .bind(old_carbon)
            .execute(&pool)
            .await?;
        assert_eq!(resolve(&pool, "carbon", "person").await?, old_carbon);
        let membership = resolve(&pool, "membership", "person[alpha]").await?;
        assert_eq!(
            resolve(&pool, "membership", "person[alpha]").await?,
            membership
        );
        let created = resolve(&pool, "silicon", "newagent:alpha").await?;
        assert_ne!(created, old_silicon);
        assert!(
            sqlx::query("INSERT INTO iam_identity_bindings VALUES('carbon','other',$1)")
                .bind(old_carbon)
                .execute(&pool)
                .await
                .is_err()
        );
        sqlx::raw_sql("CREATE SCHEMA isolated; SET search_path TO isolated")
            .execute(&pool)
            .await?;
        for migration in MIGRATOR.iter() {
            sqlx::raw_sql(migration.sql.clone()).execute(&pool).await?;
        }
        let isolated = resolve(&pool, "silicon", "agent:alpha").await?;
        assert_ne!(isolated, old_silicon);
        sqlx::raw_sql("SET search_path TO public")
            .execute(&pool)
            .await?;
        assert_eq!(resolve(&pool, "silicon", "agent:alpha").await?, old_silicon);
        pool.close().await;
        Ok(())
    }
}
