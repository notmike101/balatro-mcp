use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub struct StateDB {
    path: PathBuf,
}

impl StateDB {
    pub fn new(runtime_root: &Path) -> Self {
        Self {
            path: runtime_root.join("agent").join("rust_state.db"),
        }
    }

    fn connection(&self) -> Result<Connection, String> {
        let parent = self.path.parent().ok_or("state database has no parent")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let conn = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .map_err(|e| e.to_string())?;
        conn.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS current_run (id INTEGER PRIMARY KEY CHECK(id=1), payload TEXT NOT NULL, updated_at TEXT DEFAULT CURRENT_TIMESTAMP);
            CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY, kind TEXT NOT NULL, payload TEXT NOT NULL, created_at TEXT DEFAULT CURRENT_TIMESTAMP);
            CREATE TABLE IF NOT EXISTS strategy_rules (id TEXT PRIMARY KEY, kind TEXT NOT NULL, conditions TEXT NOT NULL, directive TEXT NOT NULL, absolute INTEGER NOT NULL DEFAULT 0, active INTEGER NOT NULL DEFAULT 1, created_at TEXT DEFAULT CURRENT_TIMESTAMP);
            CREATE TABLE IF NOT EXISTS strategy_evidence (id INTEGER PRIMARY KEY, rule_id TEXT NOT NULL, outcome TEXT NOT NULL, event_id TEXT NOT NULL, note TEXT NOT NULL, created_at TEXT DEFAULT CURRENT_TIMESTAMP);
            CREATE TABLE IF NOT EXISTS lessons (id INTEGER PRIMARY KEY, category TEXT NOT NULL, lesson TEXT NOT NULL, source TEXT NOT NULL, confidence REAL NOT NULL, created_at TEXT DEFAULT CURRENT_TIMESTAMP);
            CREATE TABLE IF NOT EXISTS estimates (id INTEGER PRIMARY KEY, hand_type TEXT NOT NULL, estimated INTEGER NOT NULL, actual INTEGER NOT NULL, error_pct REAL NOT NULL, context TEXT NOT NULL, created_at TEXT DEFAULT CURRENT_TIMESTAMP);
        "#).map_err(|e| e.to_string())?;
        Ok(conn)
    }

    pub fn checkpoint(&self, payload: &Value, kind: &str) -> Result<Value, String> {
        let conn = self.connection()?;
        let text = serde_json::to_string(payload).map_err(|e| e.to_string())?;
        conn.execute("INSERT INTO current_run(id,payload) VALUES(1,?) ON CONFLICT(id) DO UPDATE SET payload=excluded.payload, updated_at=CURRENT_TIMESTAMP", [&text]).map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO events(kind,payload) VALUES(?,?)",
            params![kind, text],
        )
        .map_err(|e| e.to_string())?;
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

    pub fn events(&self, limit: u32) -> Result<Value, String> {
        let conn = self.connection()?;
        let mut stmt = conn
            .prepare("SELECT id,kind,payload,created_at FROM events ORDER BY id DESC LIMIT ?")
            .map_err(|e| e.to_string())?;
        let rows = stmt.query_map([limit.clamp(1, 500)], |row| Ok(json!({"event_id": row.get::<_, i64>(0)?, "kind": row.get::<_, String>(1)?, "payload": serde_json::from_str::<Value>(&row.get::<_, String>(2)?).unwrap_or(Value::Null), "created_at": row.get::<_, String>(3)?}))).map_err(|e| e.to_string())?;
        Ok(Value::Array(rows.filter_map(Result::ok).collect()))
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
        conn.execute(
            "INSERT INTO strategy_rules(id,kind,conditions,directive,absolute) VALUES(?,?,?,?,?)",
            params![
                id,
                kind,
                serde_json::to_string(conditions).map_err(|e| e.to_string())?,
                directive,
                absolute as i64
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(
            json!({"id":id,"kind":kind,"conditions":conditions,"directive":directive,"absolute":absolute,"active":true}),
        )
    }

    pub fn strategy(&self) -> Result<Value, String> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT id,kind,conditions,directive,absolute,active FROM strategy_rules ORDER BY id").map_err(|e| e.to_string())?;
        let rules = stmt.query_map([], |row| Ok(json!({"id":row.get::<_,String>(0)?,"kind":row.get::<_,String>(1)?,"conditions":serde_json::from_str::<Value>(&row.get::<_,String>(2)?).unwrap_or(Value::Null),"directive":row.get::<_,String>(3)?,"absolute":row.get::<_,i64>(4)? != 0,"active":row.get::<_,i64>(5)? != 0}))).map_err(|e| e.to_string())?;
        Ok(json!({"rules": rules.filter_map(Result::ok).collect::<Vec<_>>() }))
    }

    pub fn record_evidence(
        &self,
        rule_id: &str,
        outcome: &str,
        event_id: &str,
        note: &str,
    ) -> Result<Value, String> {
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO strategy_evidence(rule_id,outcome,event_id,note) VALUES(?,?,?,?)",
            params![rule_id, outcome, event_id, note],
        )
        .map_err(|e| e.to_string())?;
        let active = !matches!(
            outcome.to_ascii_lowercase().as_str(),
            "failure" | "rejected" | "invalid"
        );
        conn.execute(
            "UPDATE strategy_rules SET active=? WHERE id=?",
            params![active as i64, rule_id],
        )
        .map_err(|e| e.to_string())?;
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
        conn.execute(
            "INSERT INTO lessons(category,lesson,source,confidence) VALUES(?,?,?,?)",
            params![category, lesson, source, confidence.clamp(0.0, 1.0)],
        )
        .map_err(|e| e.to_string())?;
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
        conn.execute(
            "INSERT INTO estimates(hand_type,estimated,actual,error_pct,context) VALUES(?,?,?,?,?)",
            params![
                hand_type,
                estimated,
                actual,
                error_pct,
                serde_json::to_string(context).map_err(|e| e.to_string())?
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(
            json!({"hand_type":hand_type,"estimated":estimated,"actual":actual,"error_pct":error_pct,"context":context}),
        )
    }

    pub fn estimation_report(&self) -> Result<Value, String> {
        let conn = self.connection()?;
        let average: f64 = conn
            .query_row(
                "SELECT COALESCE(AVG(error_pct),0) FROM estimates",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM estimates", [], |row| row.get(0))
            .map_err(|e| e.to_string())?;
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
        db.add_lesson("scoring", "check hand values", "test", 0.8)
            .unwrap();
        db.record_estimate("Pair", 100, 120, &json!({})).unwrap();
        assert_eq!(db.estimation_report().unwrap()["count"], 1);
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
}
