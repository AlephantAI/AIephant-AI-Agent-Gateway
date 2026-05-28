//! Tests for `RouterStore::get_workspace_provider_allowlist` and
//! `RouterStore::has_provider_configs_updated_since`.
//!
//! SQL-structure tests run without a DB. The integration variant that requires
//! `PostgreSQL` is skipped automatically when the DB is unavailable.
use ai_gateway::store::router::RouterStore;
use sqlx::PgPool;
use tracing::info;

const DEFAULT_TEST_DB_URL: &str = "postgres://postgres:postgres@localhost:54322/postgres";

fn preferred_test_db_url(default_url: &str) -> String {
    std::env::var("POSTGRES_DATABASE_URL")
        .or_else(|_| std::env::var("AI_GATEWAY__DATABASE__URL"))
        .unwrap_or_else(|_| default_url.to_string())
}

// ─── Integration tests (require PostgreSQL) ────────────────────────────────

/// Helper: connects to the test DB or skips the test.
async fn connect_or_skip() -> Option<PgPool> {
    let db_url = preferred_test_db_url(DEFAULT_TEST_DB_URL);
    match PgPool::connect(&db_url).await {
        Ok(pool) => Some(pool),
        Err(e) => {
            info!("skip db integration test: cannot connect to {db_url}: {e}");
            None
        }
    }
}

#[tokio::test]
async fn get_workspace_provider_allowlist_returns_map_of_enabled_rows() {
    let Some(pool) = connect_or_skip().await else {
        return;
    };

    let store = RouterStore::new(pool.clone()).expect("router store init");

    // The method must complete without error.  We don't assert exact contents
    // because the test DB seed may vary; we just verify the shape.
    let allowlist = store
        .get_workspace_provider_allowlist()
        .await
        .expect("allowlist query should succeed");

    // Every value in the map must be a non-empty set (we filter out disabled
    // entries in the query, and the helper skips unknown provider codes).
    for (workspace_id, providers) in &allowlist {
        assert!(
            !providers.is_empty(),
            "workspace {workspace_id} has an empty allowed-provider set; the \
             query should only insert known providers"
        );
    }
}

#[tokio::test]
async fn has_provider_configs_updated_since_returns_bool_without_error() {
    let Some(pool) = connect_or_skip().await else {
        return;
    };

    let store = RouterStore::new(pool.clone()).expect("router store init");

    let since = chrono::Utc::now() - chrono::Duration::hours(1);
    let result = store.has_provider_configs_updated_since(since).await;
    assert!(
        result.is_ok(),
        "has_provider_configs_updated_since should not error: {result:?}"
    );
}
