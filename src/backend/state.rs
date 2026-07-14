use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub struct StateDB {
    path: PathBuf,
}

pub const MAX_DECISION_SNAPSHOT_BYTES: usize = 64 * 1024;

type EventRow = (i64, String, Value, String);
type StrategyRow = (String, String, Value, String, bool, bool);
type EvidenceRow = (i64, String, String, String, String, String);
type LessonRow = (i64, String, String, String, f64, String);
type EstimateReportRow = (i64, f64);

fn read_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        serde_json::from_str::<Value>(&row.get::<_, String>(2)?).unwrap_or(Value::Null),
        row.get(3)?,
    ))
}

fn read_strategy_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StrategyRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        serde_json::from_str::<Value>(&row.get::<_, String>(2)?).unwrap_or(Value::Null),
        row.get(3)?,
        row.get::<_, i64>(4)? != 0,
        row.get::<_, i64>(5)? != 0,
    ))
}

fn read_evidence_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EvidenceRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}

fn read_lesson_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LessonRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}

fn read_estimate_report_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EstimateReportRow> {
    Ok((row.get(0)?, row.get(1)?))
}

fn state_sql<T>(result: rusqlite::Result<T>, context: &str) -> Result<T, String> {
    match result {
        Ok(value) => Ok(value),
        Err(error) => Err(format!("{context}: {error}")),
    }
}

fn snapshot_fallback(value: &Value, bytes: usize) -> Value {
    json!({
        "truncated": true,
        "bytes": bytes,
        "state": value
            .pointer("/game/state")
            .or_else(|| value.pointer("/state"))
            .cloned()
            .unwrap_or(Value::Null),
    })
}

fn normalize_snapshot(value: &Value) -> Value {
    let value = if let Some(object) = value.as_object() {
        let mut compact = object.clone();
        compact.remove("replay_context");
        Value::Object(compact)
    } else {
        value.clone()
    };
    let bytes = serde_json::to_vec(&value).map_or(usize::MAX, |data| data.len());
    if bytes <= MAX_DECISION_SNAPSHOT_BYTES {
        value
    } else {
        snapshot_fallback(&value, bytes)
    }
}

fn decode_snapshot(length: i64, prefix: String) -> Value {
    let bytes = usize::try_from(length).unwrap_or(usize::MAX);
    if bytes > MAX_DECISION_SNAPSHOT_BYTES {
        return json!({"truncated": true, "bytes": length});
    }
    serde_json::from_str(&prefix).unwrap_or(Value::Null)
}

impl StateDB {
    pub fn new(runtime_root: &Path) -> Self {
        Self {
            path: runtime_root.join("agent").join("rust_state.db"),
        }
    }

    fn connection(&self) -> Result<Connection, String> {
        let parent = self
            .path
            .parent()
            .expect("StateDB paths always include the agent directory");
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let conn = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .map_err(|e| e.to_string())?;
        state_sql(conn.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS current_run (id INTEGER PRIMARY KEY CHECK(id=1), payload TEXT NOT NULL, updated_at TEXT DEFAULT CURRENT_TIMESTAMP);
            CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY, kind TEXT NOT NULL, payload TEXT NOT NULL, created_at TEXT DEFAULT CURRENT_TIMESTAMP);
            CREATE TABLE IF NOT EXISTS strategy_rules (id TEXT PRIMARY KEY, kind TEXT NOT NULL, conditions TEXT NOT NULL, directive TEXT NOT NULL, absolute INTEGER NOT NULL DEFAULT 0, active INTEGER NOT NULL DEFAULT 1, created_at TEXT DEFAULT CURRENT_TIMESTAMP);
            CREATE TABLE IF NOT EXISTS strategy_evidence (id INTEGER PRIMARY KEY, rule_id TEXT NOT NULL, outcome TEXT NOT NULL, event_id TEXT NOT NULL, note TEXT NOT NULL, created_at TEXT DEFAULT CURRENT_TIMESTAMP);
            CREATE TABLE IF NOT EXISTS lessons (id INTEGER PRIMARY KEY, category TEXT NOT NULL, lesson TEXT NOT NULL, source TEXT NOT NULL, confidence REAL NOT NULL, created_at TEXT DEFAULT CURRENT_TIMESTAMP);
            CREATE TABLE IF NOT EXISTS estimates (id INTEGER PRIMARY KEY, hand_type TEXT NOT NULL, estimated INTEGER NOT NULL, actual INTEGER NOT NULL, error_pct REAL NOT NULL, context TEXT NOT NULL, created_at TEXT DEFAULT CURRENT_TIMESTAMP);
            CREATE TABLE IF NOT EXISTS decision_records (
                id INTEGER PRIMARY KEY, seed TEXT NOT NULL, ante INTEGER NOT NULL, stake INTEGER NOT NULL,
                blind_key TEXT NOT NULL, decision_id TEXT NOT NULL, action_id TEXT NOT NULL,
                action_type TEXT NOT NULL, rationale TEXT NOT NULL, alternatives TEXT,
                expected_outcome TEXT, confidence REAL, before_state TEXT NOT NULL,
                after_state TEXT NOT NULL, observed_outcome TEXT NOT NULL,
                created_at TEXT DEFAULT CURRENT_TIMESTAMP
            );
            CREATE INDEX IF NOT EXISTS idx_decision_records_lookup
                ON decision_records(seed, ante, stake, blind_key, id);"#), "state schema")?;
        Ok(conn)
    }

    pub fn checkpoint(&self, payload: &Value, kind: &str) -> Result<Value, String> {
        let conn = self.connection()?;
        let text = serde_json::to_string(payload).expect("serde_json::Value is serializable");
        state_sql(conn.execute("INSERT INTO current_run(id,payload) VALUES(1,?) ON CONFLICT(id) DO UPDATE SET payload=excluded.payload, updated_at=CURRENT_TIMESTAMP", [&text]), "current run")?;
        state_sql(
            conn.execute(
                "INSERT INTO events(kind,payload) VALUES(?,?)",
                params![kind, text],
            ),
            "event",
        )?;
        Ok(json!({"kind": kind, "event_id": conn.last_insert_rowid(), "payload": payload}))
    }

    pub fn current_run(&self) -> Result<Value, String> {
        let conn = self.connection()?;
        conn.query_row("SELECT payload FROM current_run WHERE id=1", [], |row| {
            row.get::<_, String>(0)
        })
        .map(|text| serde_json::from_str(&text).unwrap_or(Value::Null))
        .map_err(|_| "current run not found".into())
    }

    pub fn record_decision(&self, record: &Value) -> Result<Value, String> {
        let conn = self.connection()?;
        let mut stored_record = record.clone();
        if let Some(object) = stored_record.as_object_mut() {
            if let Some(before) = object.get("before_state").cloned() {
                object.insert("before_state".into(), normalize_snapshot(&before));
            }
            if let Some(after) = object.get("after_state").cloned() {
                object.insert("after_state".into(), normalize_snapshot(&after));
            }
        }
        let text = serde_json::to_string(&stored_record).expect("decision record is serializable");
        state_sql(conn.execute(
            "INSERT INTO decision_records (seed, ante, stake, blind_key, decision_id, action_id, action_type, rationale, alternatives, expected_outcome, confidence, before_state, after_state, observed_outcome) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                stored_record["seed"].as_str().unwrap_or(""),
                stored_record["ante"].as_i64().unwrap_or(0),
                stored_record["stake"].as_i64().unwrap_or(0),
                stored_record["blind_key"].as_str().unwrap_or(""),
                stored_record["decision_id"].as_str().unwrap_or(""),
                stored_record["action_id"].as_str().unwrap_or(""),
                stored_record["action_type"].as_str().unwrap_or(""),
                stored_record["why_this_action"].as_str().unwrap_or(""),
                stored_record["alternatives_considered"].as_str(),
                stored_record["expected_outcome"].as_str(),
                stored_record["confidence"].as_f64(),
                serde_json::to_string(&stored_record["before_state"]).unwrap(),
                serde_json::to_string(&stored_record["after_state"]).unwrap(),
                stored_record["observed_outcome"].as_str().unwrap_or("unknown"),
            ],
        ), "decision record")?;
        Ok(
            json!({"decision_record_id": conn.last_insert_rowid(), "record": stored_record, "stored": true, "payload": text}),
        )
    }

    pub fn decision_records(
        &self,
        seed: &str,
        ante: i64,
        stake: i64,
        blind_key: Option<&str>,
        limit: u32,
    ) -> Result<Value, String> {
        let conn = self.connection()?;
        let mut stmt = state_sql(conn.prepare(
            "SELECT seed, ante, stake, blind_key, decision_id, action_id, action_type, rationale, alternatives, expected_outcome, confidence, length(before_state), substr(before_state, 1, ?), length(after_state), substr(after_state, 1, ?), observed_outcome, created_at FROM decision_records WHERE seed=? AND ante=? AND stake=? AND (? IS NULL OR blind_key LIKE '%' || ? || '%') ORDER BY id LIMIT ?"
        ), "prepare decision records")?;
        let snapshot_limit = i64::try_from(MAX_DECISION_SNAPSHOT_BYTES).unwrap_or(i64::MAX);
        let rows = stmt.query_map(params![snapshot_limit, snapshot_limit, seed, ante, stake, blind_key, blind_key, limit.clamp(1, 500)], |row| {
            let before_len: i64 = row.get(11)?;
            let before: String = row.get(12)?;
            let after_len: i64 = row.get(13)?;
            let after: String = row.get(14)?;
            Ok(json!({
                "seed": row.get::<_, String>(0)?, "ante": row.get::<_, i64>(1)?, "stake": row.get::<_, i64>(2)?, "blind_key": row.get::<_, String>(3)?,
                "decision_id": row.get::<_, String>(4)?, "action_id": row.get::<_, String>(5)?, "action_type": row.get::<_, String>(6)?,
                "why_this_action": row.get::<_, String>(7)?, "alternatives_considered": row.get::<_, Option<String>>(8)?, "expected_outcome": row.get::<_, Option<String>>(9)?,
                "confidence": row.get::<_, Option<f64>>(10)?, "before_state": decode_snapshot(before_len, before),
                "after_state": decode_snapshot(after_len, after), "observed_outcome": row.get::<_, String>(15)?, "created_at": row.get::<_, String>(16)?
            }))
        }).map_err(|e| format!("query decision records: {e}"))?;
        Ok(Value::Array(rows.filter_map(Result::ok).collect()))
    }

    pub fn health(&self) -> Result<Value, String> {
        let bytes = fs::metadata(&self.path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let conn = self.connection()?;
        let (count, max_before, max_after): (i64, i64, i64) = state_sql(
            conn.query_row(
                "SELECT COUNT(*), COALESCE(MAX(length(before_state)), 0), COALESCE(MAX(length(after_state)), 0) FROM decision_records",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ),
            "decision health",
        )?;
        Ok(json!({
            "database_bytes": bytes,
            "decision_records": count,
            "max_before_state_bytes": max_before,
            "max_after_state_bytes": max_after,
            "snapshot_limit_bytes": MAX_DECISION_SNAPSHOT_BYTES,
        }))
    }

    pub fn events(&self, limit: u32) -> Result<Value, String> {
        let conn = self.connection()?;
        let mut stmt = state_sql(
            conn.prepare("SELECT id,kind,payload,created_at FROM events ORDER BY id DESC LIMIT ?"),
            "prepare events",
        )?;
        let rows = stmt
            .query_map([limit.clamp(1, 500)], read_event_row)
            .expect("event query parameters are statically typed");
        Ok(Value::Array(rows.filter_map(|row| {
            row.ok().map(|(event_id, kind, payload, created_at)| {
                json!({"event_id": event_id, "kind": kind, "payload": payload, "created_at": created_at})
            })
        }).collect()))
    }

    pub fn add_rule(
        &self,
        id: &str,
        kind: &str,
        conditions: &Value,
        directive: &str,
        absolute: bool,
    ) -> Result<Value, String> {
        let conn = self.connection()?;
        state_sql(conn.execute(
            "INSERT INTO strategy_rules(id,kind,conditions,directive,absolute) VALUES(?,?,?,?,?)",
            params![
                id,
                kind,
                serde_json::to_string(conditions).expect("serde_json::Value is serializable"),
                directive,
                absolute as i64
            ],
        ), "strategy rule")?;
        Ok(
            json!({"id":id,"kind":kind,"conditions":conditions,"directive":directive,"absolute":absolute,"active":true}),
        )
    }

    pub fn strategy(&self) -> Result<Value, String> {
        let conn = self.connection()?;
        let mut stmt = state_sql(conn.prepare("SELECT id,kind,conditions,directive,absolute,active FROM strategy_rules ORDER BY id"), "prepare strategy")?;
        let rules = stmt
            .query_map([], read_strategy_row)
            .map_err(|error| format!("query strategy rules: {error}"))?;
        let rules = rules
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("read strategy rules: {error}"))?
            .into_iter()
            .map(|(id, kind, conditions, directive, absolute, active)| {
                json!({"id": id, "kind": kind, "conditions": conditions, "directive": directive, "absolute": absolute, "active": active})
            })
            .collect::<Vec<_>>();
        let evidence = self.strategy_evidence(500, 0)?;
        Ok(
            json!({"rules": rules, "evidence": evidence["evidence"], "evidence_has_more": evidence["has_more"]}),
        )
    }

    fn strategy_evidence(&self, limit: u32, offset: u32) -> Result<Value, String> {
        let conn = self.connection()?;
        let mut stmt = state_sql(
            conn.prepare(
                "SELECT id,rule_id,outcome,event_id,note,created_at FROM strategy_evidence ORDER BY id DESC LIMIT ? OFFSET ?",
            ),
            "prepare strategy evidence",
        )?;
        let rows = stmt
            .query_map(
                params![i64::from(limit.clamp(1, 500)) + 1, offset],
                read_evidence_row,
            )
            .map_err(|error| format!("query strategy evidence: {error}"))?;
        let mut rows = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("read strategy evidence: {error}"))?;
        let page_size = limit.clamp(1, 500) as usize;
        let has_more = rows.len() > page_size;
        rows.truncate(page_size);
        let evidence = rows
            .into_iter()
            .map(|(id, rule_id, outcome, event_id, note, created_at)| {
                json!({
                    "id": id,
                    "rule_id": rule_id,
                    "outcome": outcome,
                    "event_id": event_id,
                    "note": note,
                    "created_at": created_at,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({"evidence": evidence, "has_more": has_more}))
    }

    pub fn lessons(&self, category: &str, limit: u32, offset: u32) -> Result<Value, String> {
        let conn = self.connection()?;
        let page_size = limit.clamp(1, 500);
        let mut stmt = state_sql(
            conn.prepare(
                "SELECT id,category,lesson,source,confidence,created_at FROM lessons WHERE (? = '' OR category = ?) ORDER BY id DESC LIMIT ? OFFSET ?",
            ),
            "prepare lessons",
        )?;
        let rows = stmt
            .query_map(
                params![category, category, i64::from(page_size) + 1, offset],
                read_lesson_row,
            )
            .map_err(|error| format!("query lessons: {error}"))?;
        let mut rows = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("read lessons: {error}"))?;
        let has_more = rows.len() > page_size as usize;
        rows.truncate(page_size as usize);
        let lessons = rows
            .into_iter()
            .map(|(id, category, lesson, source, confidence, created_at)| {
                json!({
                    "id": id,
                    "category": category,
                    "lesson": lesson,
                    "source": source,
                    "confidence": confidence,
                    "created_at": created_at,
                })
            })
            .collect::<Vec<_>>();
        let next_offset = has_more.then_some(offset.saturating_add(page_size));
        Ok(json!({
            "lessons": lessons,
            "offset": offset,
            "next_offset": next_offset,
            "has_more": has_more,
        }))
    }

    pub fn compact_recall(&self, limit: u32) -> Result<Value, String> {
        let lessons = self.lessons("", limit, 0)?;
        let evidence = self.strategy_evidence(limit, 0)?;
        Ok(json!({
            "lessons": lessons["lessons"],
            "lessons_has_more": lessons["has_more"],
            "strategy_evidence": evidence["evidence"],
            "strategy_evidence_has_more": evidence["has_more"],
            "instruction": "Review these durable lessons and evidence before repeating a strategic choice; use lesson_list or strategy_state for deeper history.",
        }))
    }

    pub fn record_evidence(
        &self,
        rule_id: &str,
        outcome: &str,
        event_id: &str,
        note: &str,
    ) -> Result<Value, String> {
        let conn = self.connection()?;
        state_sql(
            conn.execute(
                "INSERT INTO strategy_evidence(rule_id,outcome,event_id,note) VALUES(?,?,?,?)",
                params![rule_id, outcome, event_id, note],
            ),
            "strategy evidence",
        )?;
        let active = !matches!(
            outcome.to_ascii_lowercase().as_str(),
            "failure" | "rejected" | "invalid"
        );
        state_sql(
            conn.execute(
                "UPDATE strategy_rules SET active=? WHERE id=?",
                params![active as i64, rule_id],
            ),
            "strategy activation",
        )?;
        Ok(
            json!({"rule_id":rule_id,"outcome":outcome,"event_id":event_id,"note":note,"active":active}),
        )
    }

    pub fn add_lesson(
        &self,
        category: &str,
        lesson: &str,
        source: &str,
        confidence: f64,
    ) -> Result<Value, String> {
        let conn = self.connection()?;
        state_sql(
            conn.execute(
                "INSERT INTO lessons(category,lesson,source,confidence) VALUES(?,?,?,?)",
                params![category, lesson, source, confidence.clamp(0.0, 1.0)],
            ),
            "lesson",
        )?;
        Ok(
            json!({"category":category,"lesson":lesson,"source":source,"confidence":confidence.clamp(0.0,1.0)}),
        )
    }

    pub fn record_estimate(
        &self,
        hand_type: &str,
        estimated: i64,
        actual: i64,
        context: &Value,
    ) -> Result<Value, String> {
        let error_pct = if actual == 0 {
            0.0
        } else {
            ((estimated - actual).abs() as f64 / actual.abs() as f64) * 100.0
        };
        let conn = self.connection()?;
        state_sql(conn.execute(
            "INSERT INTO estimates(hand_type,estimated,actual,error_pct,context) VALUES(?,?,?,?,?)",
            params![
                hand_type,
                estimated,
                actual,
                error_pct,
                serde_json::to_string(context).expect("serde_json::Value is serializable")
            ],
        ), "estimate")?;
        Ok(
            json!({"hand_type":hand_type,"estimated":estimated,"actual":actual,"error_pct":error_pct,"context":context}),
        )
    }

    pub fn estimation_report(&self) -> Result<Value, String> {
        let conn = self.connection()?;
        let (count, average): (i64, f64) = state_sql(
            conn.query_row(
                "SELECT COUNT(*), COALESCE(AVG(error_pct),0) FROM estimates",
                [],
                read_estimate_report_row,
            ),
            "estimate report",
        )?;
        Ok(json!({"count":count,"average_error_pct":average}))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn state_round_trip_and_strategy_workflow() {
        assert!(state_sql::<()>(Ok(()), "ok").is_ok());
        assert!(state_sql::<()>(Err(rusqlite::Error::QueryReturnedNoRows), "bad").is_err());
        let dir = tempdir().unwrap();
        let db = StateDB::new(dir.path());
        let checkpoint = db.checkpoint(&json!({"state":"SHOP"}), "test").unwrap();
        assert_eq!(checkpoint["kind"], "test");
        assert_eq!(db.current_run().unwrap()["state"], "SHOP");
        assert_eq!(db.events(10).unwrap().as_array().unwrap().len(), 1);
        db.add_rule("r1", "economy", &json!({"ante": 1}), "save interest", false)
            .unwrap();
        assert_eq!(db.strategy().unwrap()["rules"].as_array().unwrap().len(), 1);
        db.record_evidence("r1", "success", "e1", "worked").unwrap();
        let strategy = db.strategy().unwrap();
        assert_eq!(strategy["evidence"].as_array().unwrap().len(), 1);
        db.add_lesson("scoring", "check hand values", "test", 0.8)
            .unwrap();
        let lessons = db.lessons("scoring", 1, 0).unwrap();
        assert_eq!(lessons["lessons"][0]["lesson"], "check hand values");
        assert_eq!(lessons["has_more"], false);
        assert_eq!(
            db.compact_recall(6).unwrap()["lessons"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        db.record_estimate("Pair", 100, 120, &json!({})).unwrap();
        assert_eq!(db.estimation_report().unwrap()["count"], 1);
    }

    #[test]
    fn decision_records_round_trip_and_filter_by_blind() {
        let dir = tempdir().unwrap();
        let db = StateDB::new(dir.path());
        db.record_decision(&json!({
            "seed": "2K9H9HN", "ante": 1, "stake": 1, "blind_key": "Small",
            "decision_id": "d1", "action_id": "play_selected", "action_type": "play",
            "why_this_action": "Best margin", "alternatives_considered": "Discard",
            "expected_outcome": "Clear", "confidence": 0.9,
            "before_state": {"state": "SELECTING_HAND"}, "after_state": {"state": "ROUND_EVAL"},
            "observed_outcome": "state_changed"
        }))
        .unwrap();
        let records = db
            .decision_records("2K9H9HN", 1, 1, Some("Small"), 10)
            .unwrap();
        assert_eq!(records[0]["why_this_action"], "Best margin");
        assert_eq!(records[0]["after_state"]["state"], "ROUND_EVAL");
        assert!(
            db.decision_records("2K9H9HN", 1, 1, Some("Boss"), 10)
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn decision_snapshots_are_bounded_for_repeated_records() {
        let dir = tempdir().unwrap();
        let db = StateDB::new(dir.path());
        for index in 0..100 {
            db.record_decision(&json!({
                "seed": "2K9H9HN", "ante": 1, "stake": 1, "blind_key": "Small",
                "decision_id": format!("d{index}"), "action_id": "play_selected",
                "action_type": "play", "why_this_action": "bounded",
                "before_state": {"state": "SELECTING_HAND", "replay_context": {"prior_decisions": ["ignored"]}},
                "after_state": {"state": "ROUND_EVAL"}, "observed_outcome": "state_changed"
            })).unwrap();
        }
        let health = db.health().unwrap();
        assert_eq!(health["decision_records"], 100);
        assert!(health["max_before_state_bytes"].as_i64().unwrap() < 1024);
        assert!(health["max_after_state_bytes"].as_i64().unwrap() < 1024);
        let records = db
            .decision_records("2K9H9HN", 1, 1, Some("Small"), 500)
            .unwrap();
        assert_eq!(records.as_array().unwrap().len(), 100);
        assert!(records[0]["before_state"].get("replay_context").is_none());
    }

    #[test]
    fn legacy_oversized_snapshots_are_returned_as_truncated_metadata() {
        let dir = tempdir().unwrap();
        let db = StateDB::new(dir.path());
        let conn = db.connection().unwrap();
        let oversized = "x".repeat(MAX_DECISION_SNAPSHOT_BYTES + 1);
        conn.execute(
            "INSERT INTO decision_records (seed, ante, stake, blind_key, decision_id, action_id, action_type, rationale, before_state, after_state, observed_outcome) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params!["2K9H9HN", 1, 1, "Small", "d1", "a1", "play", "why", oversized, "{}", "state_changed"],
        ).unwrap();
        let records = db
            .decision_records("2K9H9HN", 1, 1, Some("Small"), 10)
            .unwrap();
        assert_eq!(records[0]["before_state"]["truncated"], true);
        assert_eq!(
            records[0]["before_state"]["bytes"],
            (MAX_DECISION_SNAPSHOT_BYTES + 1) as i64
        );
    }

    #[test]
    fn state_row_decoders_cover_every_field_error() {
        macro_rules! assert_reader_error {
            ($values:expr, $reader:path) => {{
                let conn = Connection::open_in_memory().unwrap();
                let mut stmt = conn
                    .prepare(&format!("SELECT {}", $values.join(",")))
                    .unwrap();
                assert!(stmt.query_row([], $reader).is_err());
            }};
        }

        let event_values = ["1", "'kind'", "'{}'", "'time'"];
        for bad in 0..event_values.len() {
            let mut values = event_values.map(str::to_owned);
            values[bad] = "X'00'".into();
            assert_reader_error!(values, read_event_row);
        }

        let strategy_values = ["'id'", "'kind'", "'{}'", "'directive'", "0", "1"];
        for bad in 0..strategy_values.len() {
            let mut values = strategy_values.map(str::to_owned);
            values[bad] = "X'00'".into();
            assert_reader_error!(values, read_strategy_row);
        }

        let estimate_values = ["1", "1.0"];
        for bad in 0..estimate_values.len() {
            let mut values = estimate_values.map(str::to_owned);
            values[bad] = "X'00'".into();
            assert_reader_error!(values, read_estimate_report_row);
        }
    }

    #[test]
    fn empty_state_limits_and_failure_deactivation_are_deterministic() {
        let dir = tempdir().unwrap();
        let db = StateDB::new(dir.path());
        assert!(db.current_run().unwrap_err().contains("not found"));
        assert_eq!(db.events(0).unwrap().as_array().unwrap().len(), 0);
        db.add_rule("r1", "safety", &json!({}), "stop", true)
            .unwrap();
        let evidence = db.record_evidence("r1", "rejected", "e1", "bad").unwrap();
        assert_eq!(evidence["active"], false);
        assert_eq!(db.strategy().unwrap()["rules"][0]["active"], false);
        let lesson = db.add_lesson("x", "y", "z", 4.0).unwrap();
        assert_eq!(lesson["confidence"], 1.0);
        db.record_estimate("Pair", 10, 0, &json!({"source":"test"}))
            .unwrap();
        assert_eq!(db.estimation_report().unwrap()["average_error_pct"], 0.0);
    }

    #[test]
    fn malformed_persisted_json_is_safely_represented() {
        let dir = tempdir().unwrap();
        let db = StateDB::new(dir.path());
        let conn = db.connection().unwrap();
        conn.execute(
            "INSERT INTO current_run(id,payload) VALUES(1, 'broken')",
            [],
        )
        .unwrap();
        assert_eq!(db.current_run().unwrap(), Value::Null);
        conn.execute(
            "INSERT INTO events(kind,payload) VALUES('bad', 'broken')",
            [],
        )
        .unwrap();
        assert_eq!(db.events(500).unwrap()[0]["payload"], Value::Null);
    }

    #[test]
    fn database_open_errors_are_returned_by_every_workflow() {
        let dir = tempdir().unwrap();
        let bad = dir.path().join("agent/rust_state.db");
        std::fs::create_dir_all(&bad).unwrap();
        let db = StateDB::new(dir.path());
        assert!(db.checkpoint(&json!({}), "x").is_err());
        assert!(db.current_run().is_err());
        assert!(db.events(1).is_err());
        assert!(db.add_rule("x", "x", &json!({}), "x", false).is_err());
        assert!(db.strategy().is_err());
        assert!(db.record_evidence("x", "x", "x", "x").is_err());
        assert!(db.add_lesson("x", "x", "x", 0.5).is_err());
        assert!(db.record_estimate("x", 1, 2, &json!({})).is_err());
        assert!(db.estimation_report().is_err());

        let parent_file = dir.path().join("parent-file");
        std::fs::write(&parent_file, b"not a directory").unwrap();
        let db = StateDB::new(&parent_file);
        assert!(db.current_run().is_err());
    }

    #[test]
    fn locked_state_schema_is_reported() {
        let dir = tempdir().unwrap();
        let db = StateDB::new(dir.path());
        let conn = db.connection().unwrap();
        conn.execute_batch("BEGIN EXCLUSIVE").unwrap();
        assert!(db.current_run().is_err());
    }

    #[test]
    fn malformed_schema_errors_are_returned_by_every_workflow() {
        let dir = tempdir().unwrap();
        let db = StateDB::new(dir.path());
        let path = dir.path().join("agent/rust_state.db");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE current_run (wrong TEXT); CREATE TABLE events (wrong TEXT); CREATE TABLE strategy_rules (wrong TEXT); CREATE TABLE strategy_evidence (wrong TEXT); CREATE TABLE lessons (wrong TEXT); CREATE TABLE estimates (wrong TEXT);",
        )
        .unwrap();
        drop(conn);
        assert!(db.checkpoint(&json!({}), "bad").is_err());
        assert!(db.current_run().is_err());
        assert!(db.events(1).is_err());
        assert!(db.add_rule("x", "x", &json!({}), "x", false).is_err());
        assert!(db.strategy().is_err());
        assert!(db.record_evidence("x", "x", "x", "x").is_err());
        assert!(db.add_lesson("x", "x", "x", 0.5).is_err());
        assert!(db.record_estimate("x", 1, 2, &json!({})).is_err());
        assert!(db.estimation_report().is_err());
    }

    #[test]
    fn malformed_state_rows_are_safely_rejected() {
        let dir = tempdir().unwrap();
        let db = StateDB::new(dir.path());
        let conn = db.connection().unwrap();
        conn.execute("INSERT INTO current_run(id,payload) VALUES(1,X'00')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO events(id,kind,payload) VALUES(1,X'00',X'00')",
            [],
        )
        .unwrap();
        drop(conn);
        assert!(db.current_run().is_err());
        assert!(db.events(10).is_ok());
    }

    #[test]
    fn each_state_sql_failure_boundary_is_returned() {
        fn bad_db(table: &str) -> (tempfile::TempDir, StateDB) {
            let dir = tempdir().unwrap();
            let db = StateDB::new(dir.path());
            let conn = db.connection().unwrap();
            conn.execute_batch(&format!(
                "DROP TABLE {table}; CREATE TABLE {table} (wrong TEXT);"
            ))
            .unwrap();
            (dir, db)
        }

        let (_dir, db) = bad_db("current_run");
        assert!(db.checkpoint(&json!({}), "bad").is_err());
        let (_dir, db) = bad_db("events");
        assert!(db.events(1).is_err());
        assert!(db.checkpoint(&json!({}), "bad").is_err());
        let (_dir, db) = bad_db("strategy_rules");
        assert!(db.add_rule("r", "k", &json!({}), "d", false).is_err());
        assert!(db.strategy().is_err());
        assert!(db.record_evidence("r", "success", "e", "n").is_err());
        let (_dir, db) = bad_db("strategy_evidence");
        assert!(db.record_evidence("r", "success", "e", "n").is_err());
        let (_dir, db) = bad_db("lessons");
        assert!(db.add_lesson("c", "l", "s", 0.5).is_err());
        let (_dir, db) = bad_db("estimates");
        assert!(db.record_estimate("Pair", 1, 2, &json!({})).is_err());
        assert!(db.estimation_report().is_err());
    }
}
