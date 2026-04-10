//! Smoke tests for the `Database` handle and migrations.

use super::Database;

#[tokio::test]
async fn in_memory_applies_migrations() {
    let db = Database::in_memory().await.expect("open in-memory");
    // Jobs table should exist after migrations.
    let row: (i64,) = sqlx::query_as("SELECT count(*) FROM jobs")
        .fetch_one(db.pool())
        .await
        .expect("jobs table exists");
    assert_eq!(row.0, 0);
}

#[tokio::test]
async fn in_memory_instances_are_isolated() {
    let a = Database::in_memory().await.unwrap();
    let b = Database::in_memory().await.unwrap();

    sqlx::query(
        "INSERT INTO jobs (kind, payload, status, next_attempt_at, created_at, updated_at) \
         VALUES ('test', '{}', 'pending', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
    )
    .execute(a.pool())
    .await
    .unwrap();

    let (count_a,): (i64,) = sqlx::query_as("SELECT count(*) FROM jobs")
        .fetch_one(a.pool())
        .await
        .unwrap();
    let (count_b,): (i64,) = sqlx::query_as("SELECT count(*) FROM jobs")
        .fetch_one(b.pool())
        .await
        .unwrap();
    assert_eq!(count_a, 1);
    assert_eq!(count_b, 0);
}
