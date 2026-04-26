//! Unit tests for the observability module.
//!
//! Two flavours:
//!
//! 1. Pure tests against helper functions (`exponential_buckets`,
//!    `dlq_count`) — no global recorder involved.
//! 2. Render tests that drive a fresh local `PrometheusRecorder`
//!    via `with_local_recorder`. Crucially, these never touch the
//!    global recorder, so multiple tests can run in parallel without
//!    fighting over `set_global_recorder`.

use std::time::Duration;

use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

use crate::db::MIGRATIONS;

use super::{dlq_count, exponential_buckets, names};

// ---------- helpers ----------

/// Spin up an in-memory `SQLite` pool with the production migrations
/// applied. One connection only because `:memory:` is per-connection.
async fn pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("connect in-memory sqlite");
    MIGRATIONS.run(&pool).await.expect("apply migrations");
    pool
}

/// Build a fresh, *local* recorder configured exactly like the
/// production one. Returns the handle plus a guard that scopes the
/// recorder to the current thread for the closure's lifetime.
fn with_local_recorder<F: FnOnce() -> String>(f: F) -> String {
    let job_buckets = exponential_buckets(0.01, 2.0, 14);
    let ffprobe_buckets = exponential_buckets(0.05, 2.0, 14);

    let recorder = PrometheusBuilder::new()
        .set_buckets_for_metric(Matcher::Full(names::JOB_DURATION.to_owned()), &job_buckets)
        .unwrap()
        .set_buckets_for_metric(
            Matcher::Full(names::FFPROBE_DURATION.to_owned()),
            &ffprobe_buckets,
        )
        .unwrap()
        .build_recorder();
    let handle = recorder.handle();

    metrics::with_local_recorder(&recorder, || {
        // Production install also describes everything; mirror it so
        // render tests see the # HELP lines.
        super::describe_all();
        let _ = f();
    });
    handle.render()
}

// ---------- exponential_buckets ----------

#[test]
fn exponential_buckets_shape() {
    let b = exponential_buckets(0.01, 2.0, 14);
    assert_eq!(b.len(), 14);
    assert!((b[0] - 0.01).abs() < 1e-12);
    assert!((b[1] - 0.02).abs() < 1e-12);
    // Last bucket should be 0.01 * 2^13 = 81.92.
    assert!((b[13] - 81.92).abs() < 1e-9);
}

#[test]
fn exponential_buckets_ffprobe_shape() {
    let b = exponential_buckets(0.05, 2.0, 14);
    assert_eq!(b.len(), 14);
    assert!((b[0] - 0.05).abs() < 1e-12);
    // Last bucket should be 0.05 * 2^13 = 409.6.
    assert!((b[13] - 409.6).abs() < 1e-9);
}

// ---------- dlq_count ----------

#[tokio::test]
async fn dlq_count_empty_table_is_zero() {
    let pool = pool().await;
    let n = dlq_count(&pool).await.unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn dlq_count_seeded_rows() {
    let pool = pool().await;
    // Three dead-letter, two alive. Only the dead-letter rows count.
    let now = chrono::Utc::now().to_rfc3339();
    for status in [
        "dead_letter",
        "dead_letter",
        "dead_letter",
        "pending",
        "completed",
    ] {
        sqlx::query(
            "INSERT INTO jobs (kind, payload, status, attempts, max_attempts, \
                 next_attempt_at, created_at, updated_at) \
             VALUES ('test_kind', '{}', ?, 0, 5, ?, ?, ?)",
        )
        .bind(status)
        .bind(&now)
        .bind(&now)
        .bind(&now)
        .execute(&pool)
        .await
        .unwrap();
    }
    let n = dlq_count(&pool).await.unwrap();
    assert_eq!(n, 3);
}

// ---------- render: ffprobe histogram with outcome label ----------

#[test]
fn ffprobe_histogram_renders_with_outcome_label() {
    let render = with_local_recorder(|| {
        metrics::histogram!(names::FFPROBE_DURATION, "outcome" => "success").record(0.123);
        String::new()
    });

    // Real histogram, not summary.
    assert!(
        render.contains("# TYPE multilinguarr_ffprobe_duration_seconds histogram"),
        "expected histogram TYPE line in:\n{render}"
    );
    // Outcome label propagated all the way through.
    assert!(
        render.contains("multilinguarr_ffprobe_duration_seconds_count{outcome=\"success\"}"),
        "expected outcome=success on _count line in:\n{render}"
    );
    // HELP description present.
    assert!(
        render.contains("# HELP multilinguarr_ffprobe_duration_seconds"),
        "expected HELP line in:\n{render}"
    );
}

#[test]
fn ffprobe_histogram_renders_with_timeout_outcome() {
    let render = with_local_recorder(|| {
        metrics::histogram!(names::FFPROBE_DURATION, "outcome" => "timeout").record(30.0);
        String::new()
    });
    assert!(
        render.contains("multilinguarr_ffprobe_duration_seconds_count{outcome=\"timeout\"}"),
        "expected outcome=timeout label in:\n{render}"
    );
}

// ---------- render: lowercased strategy label on links_created ----------

#[test]
fn links_created_strategy_label_is_lowercase() {
    let render = with_local_recorder(|| {
        // Mirror what import.rs emits after the refactor.
        metrics::counter!(
            names::LINKS_CREATED,
            "instance" => "sonarr-fr",
            "strategy" => "symlink",
        )
        .increment(1);
        metrics::counter!(
            names::LINKS_CREATED,
            "instance" => "radarr-en",
            "strategy" => "hardlink",
        )
        .increment(2);
        String::new()
    });

    assert!(
        render.contains("strategy=\"symlink\""),
        "expected lowercase symlink in:\n{render}"
    );
    assert!(
        render.contains("strategy=\"hardlink\""),
        "expected lowercase hardlink in:\n{render}"
    );
    // Hard guard against the old Debug-formatted form.
    assert!(
        !render.contains("strategy=\"Symlink\""),
        "old PascalCase strategy value leaked into render:\n{render}"
    );
    assert!(
        !render.contains("strategy=\"Hardlink\""),
        "old PascalCase strategy value leaked into render:\n{render}"
    );
}

// ---------- render: dead_letter_jobs gauge from a tick read ----------

#[tokio::test]
async fn dlq_gauge_reflects_table_count() {
    let pool = pool().await;
    let now = chrono::Utc::now().to_rfc3339();
    for _ in 0..2 {
        sqlx::query(
            "INSERT INTO jobs (kind, payload, status, attempts, max_attempts, \
                 next_attempt_at, created_at, updated_at) \
             VALUES ('test_kind', '{}', 'dead_letter', 0, 5, ?, ?, ?)",
        )
        .bind(&now)
        .bind(&now)
        .bind(&now)
        .execute(&pool)
        .await
        .unwrap();
    }

    let count = dlq_count(&pool).await.unwrap();
    assert_eq!(count, 2);

    // Drive the gauge through a local recorder (no spawn, no timer).
    let render = with_local_recorder(|| {
        #[allow(clippy::cast_precision_loss)]
        metrics::gauge!(names::DEAD_LETTER_JOBS).set(count as f64);
        String::new()
    });
    assert!(
        render.contains("# TYPE multilinguarr_dead_letter_jobs gauge"),
        "expected gauge TYPE line in:\n{render}"
    );
    assert!(
        render.contains("multilinguarr_dead_letter_jobs 2"),
        "expected gauge value 2 in:\n{render}"
    );
}

// ---------- spawn_dlq_tick: cancellation works ----------

#[tokio::test]
async fn dlq_tick_exits_on_cancel() {
    let pool = pool().await;
    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = super::spawn_dlq_tick(pool, cancel.clone());
    cancel.cancel();
    // Should join promptly — give it a generous bound to avoid flakes.
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("tick task should exit after cancel")
        .expect("task should not panic");
}
