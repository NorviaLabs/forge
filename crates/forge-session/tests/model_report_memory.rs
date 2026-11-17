//! The report's memory does not grow with every turn it reads: rows are
//! streamed into running totals, and only the two numbers per turn the
//! medians need are kept. (Reading a 50,000-turn journal used to peak at
//! about 300 MB.)
//!
//! Its own test binary: the counting allocator below is global.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use forge_durable::{new_session_id, Journal};
use forge_session::model_report::{model_report_in, ReportOptions};
use forge_test_support::turn_records::TurnRecordBuilder;
use forge_types::TaskKind;

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let now = CURRENT.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const TURNS: i64 = 50_000;

#[tokio::test]
async fn reporting_on_a_large_journal_keeps_memory_small() {
    let dir = tempfile::tempdir().unwrap();
    let session_id = new_session_id();
    let journal = Journal::open(dir.path(), session_id).await.unwrap();
    journal
        .append_turn_record(
            &TurnRecordBuilder::new(TaskKind::Main, "anthropic/claude-opus-4-1")
                .session_id(session_id)
                .task_id(0)
                .ended_at(chrono::Utc::now())
                .effort(Some("high"))
                .tokens(Some(1), Some(1))
                .model_calls(None)
                .duration_ms(Some(1))
                .build(),
        )
        .await
        .unwrap();
    drop(journal);
    let path = dir.path().join(format!("{session_id}.db"));
    {
        use sqlx::{ConnectOptions, Connection};
        let mut conn = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .connect()
            .await
            .unwrap();
        // Rows like real ones: several signals with details each.
        sqlx::query(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?) \
             INSERT INTO turn_records (session_id, task_id, started_at, ended_at, model, \
             profile_id, reasoning_effort, outcome, completion_reason, escalated, \
             escalation_retry, prompt_tokens, completion_tokens, cache_read_tokens, \
             cache_write_tokens, model_calls, duration_ms, waiting_ms, signals, task_kind) \
             SELECT ?, i, '2025-06-01T00:00:00+00:00', '2025-06-01T00:01:00+00:00', \
             'anthropic/claude-opus-4-1', 'anthropic', 'high', 'completed', 'edit_verified', \
             0, 0, 1000 + i, 200, 0, 0, 3, 60000 + i, 0, \
             '[{\"kind\":\"verification_passed\",\"detail\":\"cargo test --workspace\",\"at_ms\":1200},' \
             || '{\"kind\":\"approval_requested\",\"detail\":\"git push origin main\",\"at_ms\":3400},' \
             || '{\"kind\":\"approval_granted\",\"detail\":\"git push origin main\",\"at_ms\":5600}]', \
             'main' FROM n",
        )
        .bind(TURNS)
        .bind(session_id.to_string())
        .execute(&mut conn)
        .await
        .unwrap();
        conn.close().await.unwrap();
    }

    let before = CURRENT.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let report = model_report_in(dir.path(), &ReportOptions::default())
        .await
        .unwrap();
    let peak = PEAK.load(Ordering::Relaxed) - before;
    assert_eq!(report.kinds[0].turns as i64, TURNS + 1);
    // Two f64 samples per turn is 0.8 MB; everything else is per group or
    // per page of rows.
    assert!(
        peak < 16 * 1024 * 1024,
        "reading {TURNS} turns peaked at {:.1} MB",
        peak as f64 / 1_048_576.0
    );
}
