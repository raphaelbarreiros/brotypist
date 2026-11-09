//! Public-API encrypted learning smoke with fixed synthetic EN/PT fixtures only.
//!
//! Owns one private temporary directory; never opens a normal profile, credential
//! backend, capture source, or model. SQLite is used directly only for synthetic
//! legacy/damaged payloads, locks and write refusals. Receipts are content-free.
//! Warm/cold equality establishes behavioral equivalence, not cache work counts.
use std::{path::Path, process::ExitCode};

use anyhow::{Result, ensure};
use brotypist::storage::{HistoryStore, seal_record};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

const KEY: [u8; 32] = [0x5a; 32];
const EN: &str = "The quartzwork plan is ready and we review quartzwork with our team before the meeting. We discuss quartzwork and the next steps for our work.";
const REPLACEMENT: &str = "The cedarwork plan is ready and we review cedarwork with our team before the meeting. We discuss cedarwork and the next steps for our work.";
const PT: &str = "Nós discutimos observabilidade com nossa equipe durante a reunião para organizar uma próxima etapa. A observabilidade é importante para nossa revisão e mais observabilidade durante uma próxima reunião.";
const GENERATED: &str = "😀 e\u{301} phantomword phantomword phantomword. ";
const EN_QUERY: &str = "The plan is ready and we review our work";
const PT_QUERY: &str = "Nós discutimos nossa próxima reunião com uma equipe";
const QUERIES: &[(&str, &str, &str)] = &[
    ("writer", "", EN_QUERY),
    ("writer", "", PT_QUERY),
    ("other", "", EN_QUERY),
    ("browser", "", EN_QUERY),
    ("browser", "chat.example", EN_QUERY),
    ("browser-peer", "chat.example", EN_QUERY),
    ("browser", "sub.chat.example", EN_QUERY),
    ("browser", "other.example", EN_QUERY),
];

struct Receipt {
    stage: &'static str,
    outcomes: Vec<Value>,
    cleanup: bool,
}

impl Receipt {
    fn pass(&mut self, count: usize) {
        self.outcomes.push(json!({
            "scenario": self.stage,
            "outcome": "passed",
            "observed_count": count,
        }));
    }
}

fn record(
    store: &mut HistoryStore,
    id: &str,
    app: &str,
    domain: &str,
    text: &str,
    spans: &[(usize, usize)],
) -> Result<()> {
    let time = DateTime::parse_from_rfc3339("2026-01-02T03:04:05Z")?.with_timezone(&Utc);
    ensure!(store.record_identified_at(id, app, domain, text, spans, time)?);
    Ok(())
}

fn authored(store: &mut HistoryStore, id: &str, app: &str, domain: &str, text: &str) -> Result<()> {
    record(store, id, app, domain, text, &[(0, text.chars().count())])
}

fn learn(store: &HistoryStore, app: &str, domain: &str, prefix: &str) -> Result<String> {
    store.personalization(app, domain, prefix, 100, &|_, _| true)
}

fn evidence(raw: &str, language: &str, count: u64, present: &str, absent: &str) -> Result<()> {
    let value: Value = serde_json::from_str(raw)?;
    ensure!(value["language"] == language);
    ensure!(value["records"] == count);
    let words = value["vocabulary"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Missing vocabulary"))?;
    ensure!(words.iter().any(|word| word.as_str() == Some(present)));
    ensure!(!words.iter().any(|word| word.as_str() == Some(absent)));
    Ok(())
}

// A new store has no shared learning index. Compare public records and every
// scoped retrieval, rather than reading implementation counters or cache state.
fn fresh_oracle(store: &HistoryStore) -> Result<usize> {
    let mut oracle = HistoryStore::with_path_and_key(store.path(), KEY);
    oracle.unlock()?;
    ensure!(store.records(None, None, 100)? == oracle.records(None, None, 100)?);
    for &(app, domain, prefix) in QUERIES {
        ensure!(learn(store, app, domain, prefix)? == learn(&oracle, app, domain, prefix)?);
    }
    Ok(QUERIES.len())
}

fn exercise(path: &Path, receipt: &mut Receipt) -> Result<()> {
    let mut store = HistoryStore::with_path_and_key(path, KEY);
    store.unlock()?;

    receipt.stage = "authored_only_evidence";
    record(&mut store, "generated", "writer", "", EN, &[])?;
    ensure!(store.records(None, None, 100)?.len() == 1);
    ensure!(learn(&store, "writer", "", EN_QUERY)?.is_empty());
    let mixed = format!("{GENERATED}{EN}");
    let start = GENERATED.chars().count();
    let spans = [(start, start + EN.chars().count())];
    record(&mut store, "session", "writer", "", &mixed, &spans)?;
    let rows = store.records(Some("writer"), None, 100)?;
    let session = rows
        .iter()
        .find(|row| row.id == "session")
        .ok_or_else(|| anyhow::anyhow!("Missing record"))?;
    ensure!(session.text == mixed && session.authored_spans == spans);
    let initial = learn(&store, "writer", "", EN_QUERY)?;
    evidence(&initial, "en", 1, "quartzwork", "phantomword")?;
    // Removing generated surroundings must leave every public derived feature
    // unchanged, including style, not just the marker vocabulary.
    authored(&mut store, "session", "writer", "", EN)?;
    ensure!(learn(&store, "writer", "", EN_QUERY)? == initial);
    receipt.pass(rows.len());

    receipt.stage = "warm_retrieval";
    for _ in 0..3 {
        ensure!(learn(&store, "writer", "", EN_QUERY)? == initial);
    }
    receipt.pass(fresh_oracle(&store)?);

    receipt.stage = "stable_id_replacement";
    authored(&mut store, "session", "writer", "", REPLACEMENT)?;
    let replaced = learn(&store, "writer", "", EN_QUERY)?;
    evidence(&replaced, "en", 1, "cedarwork", "quartzwork")?;
    ensure!(replaced != initial);
    let rows = store.records(None, None, 100)?;
    ensure!(rows.len() == 2);
    ensure!(rows.iter().filter(|row| row.id == "session").count() == 1);
    ensure!(
        rows.iter()
            .find(|row| row.id == "session")
            .is_some_and(|row| row.text == REPLACEMENT)
    );
    fresh_oracle(&store)?;
    receipt.pass(rows.len());

    receipt.stage = "dirty_incremental_refresh";
    authored(&mut store, "portuguese", "writer", "", PT)?;
    evidence(
        &learn(&store, "writer", "", PT_QUERY)?,
        "pt",
        1,
        "observabilidade",
        "cedarwork",
    )?;
    ensure!(learn(&store, "writer", "", EN_QUERY)? == replaced);
    receipt.pass(fresh_oracle(&store)?);

    receipt.stage = "exact_app_and_domain_separation";
    authored(&mut store, "other-local", "other", "", EN)?;
    authored(&mut store, "host", "browser", "chat.example", EN)?;
    authored(
        &mut store,
        "subhost",
        "browser",
        "sub.chat.example",
        REPLACEMENT,
    )?;
    evidence(
        &learn(&store, "other", "", EN_QUERY)?,
        "en",
        1,
        "quartzwork",
        "cedarwork",
    )?;
    ensure!(learn(&store, "writer", "", EN_QUERY)? == replaced);
    ensure!(learn(&store, "unknown", "", EN_QUERY)?.is_empty());
    ensure!(learn(&store, "browser", "", EN_QUERY)?.is_empty());
    let host = learn(&store, "browser", "chat.example", EN_QUERY)?;
    evidence(&host, "en", 1, "quartzwork", "cedarwork")?;
    // Website evidence is exact-host, shared across applications by contract;
    // domainless evidence is app-local. A subdomain is a separate host.
    ensure!(learn(&store, "browser-peer", "chat.example", EN_QUERY)? == host);
    evidence(
        &learn(&store, "browser", "sub.chat.example", EN_QUERY)?,
        "en",
        1,
        "cedarwork",
        "quartzwork",
    )?;
    ensure!(learn(&store, "browser", "other.example", EN_QUERY)?.is_empty());
    ensure!(store.records(None, Some("chat.example"), 100)?.len() == 1);
    ensure!(store.records(None, Some("sub.chat.example"), 100)?.len() == 1);
    receipt.pass(fresh_oracle(&store)?);

    receipt.stage = "consent_withdrawal";
    ensure!(
        store
            .personalization("writer", "", EN_QUERY, 100, &|_, _| false)?
            .is_empty()
    );
    // Permit the requester, but withdraw consent from the stored source app.
    ensure!(
        store
            .personalization("browser-peer", "chat.example", EN_QUERY, 100, &|app, _| app
                != "browser")?
            .is_empty()
    );
    ensure!(
        store
            .personalization("writer", "", EN_QUERY, 0, &|_, _| true)?
            .is_empty()
    );
    ensure!(learn(&store, "writer", "", EN_QUERY)? == replaced);
    ensure!(learn(&store, "browser-peer", "chat.example", EN_QUERY)? == host);
    // Policy withdrawal stops influence; explicit forgetting below removes data.
    ensure!(store.records(None, None, 100)?.len() == 6);
    receipt.pass(3);

    receipt.stage = "scoped_forgetting";
    let domain_deleted = store.delete(None, Some("chat.example"))?;
    ensure!(domain_deleted == 1);
    ensure!(store.records(None, Some("chat.example"), 100)?.is_empty());
    ensure!(learn(&store, "browser", "chat.example", EN_QUERY)?.is_empty());
    ensure!(learn(&store, "browser-peer", "chat.example", EN_QUERY)?.is_empty());
    evidence(
        &learn(&store, "browser", "sub.chat.example", EN_QUERY)?,
        "en",
        1,
        "cedarwork",
        "quartzwork",
    )?;
    let app_deleted = store.delete(Some("writer"), None)?;
    ensure!(app_deleted == 3);
    ensure!(store.records(Some("writer"), None, 100)?.is_empty());
    ensure!(learn(&store, "writer", "", EN_QUERY)?.is_empty());
    ensure!(learn(&store, "writer", "", PT_QUERY)?.is_empty());
    evidence(
        &learn(&store, "other", "", EN_QUERY)?,
        "en",
        1,
        "quartzwork",
        "cedarwork",
    )?;
    ensure!(store.records(None, None, 100)?.len() == 2);
    fresh_oracle(&store)?;
    receipt.pass(domain_deleted + app_deleted);

    receipt.stage = "locked_store_refusal";
    store.lock();
    ensure!(store.records(None, None, 100).is_err());
    ensure!(learn(&store, "other", "", EN_QUERY)?.is_empty());
    ensure!(!store.record("other", "", EN, &[(0, EN.chars().count())])?);
    ensure!(store.delete(Some("other"), None).is_err());
    store.unlock()?;
    ensure!(store.records(None, None, 100)?.len() == 2);
    fresh_oracle(&store)?;
    receipt.pass(4);

    receipt.stage = "sqlite_lock_refusal_and_recovery";
    authored(&mut store, "dirty-lock", "other", "", REPLACEMENT)?;
    let connection = Connection::open(path)?;
    connection.execute_batch("BEGIN EXCLUSIVE")?;
    let blocked_read = learn(&store, "other", "", EN_QUERY);
    let blocked_write = store.record("other", "", EN, &[(0, EN.chars().count())]);
    connection.execute_batch("ROLLBACK")?;
    drop(connection);
    ensure!(blocked_read.is_err() && blocked_write.is_err());
    ensure!(store.is_unlocked());
    ensure!(store.records(None, None, 100)?.len() == 3);
    evidence(
        &learn(&store, "other", "", EN_QUERY)?,
        "en",
        2,
        "quartzwork",
        "phantomword",
    )?;
    fresh_oracle(&store)?;
    receipt.pass(2);

    receipt.stage = "changed_ciphertext_authentication_refusal";
    // Warm the existing row, then dirty the cache through a real public write.
    // Mutating the old row under the same id tests changed-ciphertext handling,
    // not merely first-time cold authentication of a malformed new record.
    ensure!(!learn(&store, "other", "", EN_QUERY)?.is_empty());
    authored(&mut store, "refresh-trigger", "other", "", EN)?;
    let connection = Connection::open(path)?;
    let mut ciphertext: Vec<u8> = connection.query_row(
        "SELECT sealed FROM history WHERE id=?",
        ["other-local"],
        |row| row.get(0),
    )?;
    let last = ciphertext
        .last_mut()
        .ok_or_else(|| anyhow::anyhow!("Missing ciphertext"))?;
    *last ^= 1;
    ensure!(
        connection.execute(
            "UPDATE history SET sealed=? WHERE id=?",
            params![ciphertext, "other-local"],
        )? == 1
    );
    drop(connection);
    ensure!(learn(&store, "other", "", EN_QUERY).is_err());
    ensure!(!store.is_unlocked());
    ensure!(learn(&store, "other", "", EN_QUERY)?.is_empty());
    ensure!(store.records(None, None, 100).is_err());
    ensure!(store.unlock().is_err());
    ensure!(!store.record("other", "", EN, &[(0, EN.chars().count())])?);
    ensure!(store.delete(Some("other"), None).is_err());
    let mut oracle = HistoryStore::with_path_and_key(path, KEY);
    oracle.unlock()?;
    // An unrelated filter must not conceal damaged writing from record reads.
    ensure!(oracle.records(Some("missing-app"), None, 1).is_err());
    ensure!(oracle.unlock().is_err());
    drop(oracle);
    let mut oracle = HistoryStore::with_path_and_key(path, KEY);
    oracle.unlock()?;
    ensure!(learn(&oracle, "other", "", EN_QUERY).is_err());
    drop(oracle);
    receipt.pass(1);

    receipt.stage = "full_delete_recovery";
    ensure!(store.delete(None, None)? == 4);
    store.unlock()?;
    ensure!(store.records(None, None, 100)?.is_empty());
    for &(app, domain, prefix) in QUERIES {
        ensure!(learn(&store, app, domain, prefix)?.is_empty());
    }
    fresh_oracle(&store)?;
    authored(&mut store, "recovered", "writer", "", PT)?;
    evidence(
        &learn(&store, "writer", "", PT_QUERY)?,
        "pt",
        1,
        "observabilidade",
        "quartzwork",
    )?;
    ensure!(store.records(None, None, 100)?.len() == 1);
    fresh_oracle(&store)?;
    store.lock();
    ensure!(store.delete(None, None)? == 1);
    store.unlock()?;
    ensure!(store.records(None, None, 100)?.is_empty());
    ensure!(learn(&store, "writer", "", PT_QUERY)?.is_empty());
    fresh_oracle(&store)?;
    receipt.pass(5);
    Ok(())
}

fn exercise_retention(path: &Path, receipt: &mut Receipt) -> Result<()> {
    let cutoff: DateTime<Utc> = "2026-01-03T03:04:05Z".parse()?;
    let mut store = HistoryStore::with_path_and_key(path, KEY);
    receipt.stage = "retention_locked_empty_refusal";
    ensure!(store.retain_since(cutoff).is_err());
    ensure!(!path.exists() && !store.is_unlocked());
    store.unlock()?;
    ensure!(store.retain_since(cutoff)?.deleted_count() == 0);
    ensure!(!path.exists());
    receipt.pass(2);

    receipt.stage = "retention_age_boundaries_unicode_and_scopes";
    let unicode = format!("😀 e\u{301} {PT}");
    for (id, app, domain, text, time) in [
        (
            "old-😀",
            "writer",
            "",
            EN,
            cutoff - chrono::Duration::microseconds(1),
        ),
        (
            "old-host",
            "browser",
            "chat.example",
            EN,
            cutoff - chrono::Duration::days(1),
        ),
        ("equal", "writer", "", REPLACEMENT, cutoff),
        (
            "new-e\u{301}",
            "other",
            "",
            unicode.as_str(),
            cutoff + chrono::Duration::microseconds(1),
        ),
        (
            "new-host",
            "browser",
            "sub.chat.example",
            PT,
            cutoff + chrono::Duration::days(1),
        ),
    ] {
        ensure!(store.record_identified_at(
            id,
            app,
            domain,
            text,
            &[(0, text.chars().count())],
            time
        )?);
    }
    let connection = Connection::open(path)?;
    for (id, value) in [
        ("legacy-missing", json!({"text": EN, "app": "legacy"})),
        ("legacy-invalid", json!({"text": EN, "time": "invalid"})),
        (
            "legacy-naive",
            json!({"text": EN, "time": "2020-01-01T00:00:00"}),
        ),
        ("legacy-type", json!({"text": EN, "time": 17})),
        (
            "offset-old",
            json!({"text": EN, "time": "2026-01-03T04:04:04+01:00"}),
        ),
    ] {
        let sealed = seal_record(&KEY, id, &serde_json::to_vec(&value)?)?;
        connection.execute("INSERT INTO history VALUES (?,?)", params![id, sealed])?;
    }
    drop(connection);
    store.record_statistics_at("writer", EN, true, cutoff.date_naive())?;
    let statistics = store.statistics_at(7, "app", cutoff.date_naive())?;
    evidence(
        &learn(&store, "writer", "", EN_QUERY)?,
        "en",
        2,
        "quartzwork",
        "phantomword",
    )?;
    ensure!(!learn(&store, "browser-peer", "chat.example", EN_QUERY)?.is_empty());
    let before = store.records(None, None, 100)?;
    let result = store.retain_since(cutoff)?;
    ensure!(result.deleted_count() == 3 && result.unknown_timestamp_count == 4);
    let mut actual_ids = result.deleted_ids;
    actual_ids.sort();
    ensure!(actual_ids == ["offset-old", "old-host", "old-😀"]);
    // Exact IDs are propagation inputs only, never included in the receipt.
    // Every surviving row is byte-identical, not merely a matching row count.
    let expected: Vec<_> = before
        .into_iter()
        .filter(|row| !actual_ids.contains(&row.id))
        .collect();
    ensure!(store.records(None, None, 100)? == expected);
    evidence(
        &learn(&store, "writer", "", EN_QUERY)?,
        "en",
        1,
        "cedarwork",
        "quartzwork",
    )?;
    ensure!(learn(&store, "browser-peer", "chat.example", EN_QUERY)?.is_empty());
    ensure!(store.records(Some("other"), None, 100)?[0].text == unicode);
    ensure!(store.statistics_at(7, "app", cutoff.date_naive())? == statistics);
    let repeated = store.retain_since(cutoff)?;
    ensure!(repeated.deleted_count() == 0 && repeated.unknown_timestamp_count == 4);
    fresh_oracle(&store)?;
    receipt.pass(actual_ids.len());

    receipt.stage = "retention_locked_existing_refusal";
    store.lock();
    ensure!(store.retain_since(cutoff).is_err());
    ensure!(!store.is_unlocked());
    store.unlock()?;
    ensure!(store.records(None, None, 100)? == expected);
    receipt.pass(1);

    receipt.stage = "retention_atomic_write_refusal_and_recovery";
    authored(&mut store, "expired-first", "writer", "", EN)?;
    authored(&mut store, "expired-second", "writer", "", EN)?;
    let before = store.records(None, None, 100)?;
    let connection = Connection::open(path)?;
    connection.execute_batch(
        "CREATE TRIGGER refuse_retention BEFORE DELETE ON history
         WHEN OLD.id='expired-first' BEGIN SELECT RAISE(ABORT, 'synthetic refusal'); END;",
    )?;
    ensure!(store.retain_since(cutoff).is_err());
    ensure!(store.is_unlocked());
    ensure!(store.records(None, None, 100)? == before);
    fresh_oracle(&store)?;
    connection.execute_batch("DROP TRIGGER refuse_retention; BEGIN EXCLUSIVE")?;
    let blocked = store.retain_since(cutoff);
    connection.execute_batch("ROLLBACK")?;
    drop(connection);
    ensure!(blocked.is_err() && store.is_unlocked());
    ensure!(store.records(None, None, 100)? == before);
    ensure!(store.retain_since(cutoff)?.deleted_count() == 2);
    evidence(
        &learn(&store, "writer", "", EN_QUERY)?,
        "en",
        1,
        "cedarwork",
        "quartzwork",
    )?;
    receipt.pass(2);

    receipt.stage = "retention_authentication_atomicity_and_latch";
    authored(&mut store, "expired", "writer", "", EN)?;
    let count = store.records(None, None, 100)?.len();
    ensure!(!learn(&store, "writer", "", EN_QUERY)?.is_empty());
    let connection = Connection::open(path)?;
    connection.execute(
        "UPDATE history SET sealed=? WHERE id='new-host'",
        [vec![0u8; 32]],
    )?;
    drop(connection);
    ensure!(store.retain_since(cutoff).is_err());
    ensure!(!store.is_unlocked() && store.unlock().is_err());
    ensure!(store.retain_since(cutoff).is_err());
    ensure!(store.history_info()?["count"] == count);
    ensure!(learn(&store, "writer", "", EN_QUERY)?.is_empty());
    ensure!(store.delete(None, None)? == count);
    store.unlock()?;
    ensure!(store.retain_since(cutoff)?.deleted_count() == 0);
    ensure!(store.statistics_at(7, "app", cutoff.date_naive())? == statistics);
    receipt.pass(count);

    receipt.stage = "retention_credential_refusal_no_implicit_retry";
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    let mut refused = HistoryStore::with_key_loader(
        path,
        Box::new(move || {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            anyhow::bail!("Synthetic credential refusal")
        }),
    );
    ensure!(refused.retain_since(cutoff).is_err());
    ensure!(calls.load(std::sync::atomic::Ordering::SeqCst) == 0);
    ensure!(refused.unlock().is_err());
    ensure!(refused.retain_since(cutoff).is_err());
    ensure!(calls.load(std::sync::atomic::Ordering::SeqCst) == 1);
    receipt.pass(1);
    Ok(())
}

fn run(receipt: &mut Receipt) -> Result<()> {
    let temporary = tempfile::Builder::new()
        .prefix("brotypist-learning-smoke-")
        .tempdir()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700))?;
    }
    let result = exercise(&temporary.path().join("synthetic.sqlite3"), receipt)
        .and_then(|()| exercise_retention(&temporary.path().join("retention.sqlite3"), receipt));
    // All connections/stores have dropped before removing only our directory.
    let cleanup = temporary.close();
    receipt.cleanup = cleanup.is_ok();
    result?;
    receipt.stage = "owned_temporary_cleanup";
    cleanup?;
    receipt.pass(1);
    Ok(())
}

fn main() -> ExitCode {
    let mut receipt = Receipt {
        stage: "private_fixture_setup",
        outcomes: Vec::new(),
        cleanup: false,
    };
    let passed = run(&mut receipt).is_ok();
    // Never print error chains, database paths, writing, keys or derived words.
    println!(
        "{}",
        json!({
            "smoke": "learning_store",
            "fixture": "synthetic_en_pt",
            "status": if passed { "passed" } else { "failed" },
            "failed_scenario": if passed { None } else { Some(receipt.stage) },
            "scenarios": receipt.outcomes,
            "temporary_cleanup": receipt.cleanup,
        })
    );
    if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
