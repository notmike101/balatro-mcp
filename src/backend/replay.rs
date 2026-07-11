use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::PathBuf;

const CLEAR: &str = "clear";
const FAIL: &str = "fail";

pub struct ReplayDB {
    db_path: PathBuf,
}

impl ReplayDB {
    pub fn new(runtime_root: &std::path::Path) -> Self {
        let db_path = runtime_root.join("agent").join("replays.db");
        Self { db_path }
    }

    fn open(&self) -> Result<Connection, String> {
        let parent = self
            .db_path
            .parent()
            .ok_or("replay database has no parent")?;
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create replay DB directory: {e}"))?;
        Connection::open_with_flags(
            &self.db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .map_err(|e| format!("cannot open replay DB: {e}"))
    }

    fn init_db(&self, conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            r#"CREATE TABLE IF NOT EXISTS replay (
                id INTEGER PRIMARY KEY, seed TEXT NOT NULL, ante INTEGER NOT NULL,
                stake INTEGER NOT NULL, blind_key TEXT NOT NULL, outcome TEXT NOT NULL,
                chips_required INTEGER, max_chips_gained INTEGER,
                created_at TEXT DEFAULT (datetime('now')))"#,
        )
        .map_err(|e| format!("replay table: {e}"))?;
        conn.execute_batch(
            r#"CREATE TABLE IF NOT EXISTS replay_step (
                id INTEGER PRIMARY KEY, replay_id INTEGER NOT NULL, step_order INTEGER,
                action_type TEXT NOT NULL, details TEXT NOT NULL, rationale TEXT,
                hand_type TEXT, cards_held TEXT, cards_discarded TEXT, discard_count INTEGER DEFAULT 0,
                final_cards TEXT, base_chips INTEGER, base_mult INTEGER, final_score INTEGER,
                consumable_name TEXT, consumable_target_hand TEXT, notes TEXT,
                FOREIGN KEY (replay_id) REFERENCES replay(id))"#
        ).map_err(|e| format!("replay_step table: {e}"))?;
        conn.execute_batch(
            r#"CREATE TABLE IF NOT EXISTS replay_joker_config (
                id INTEGER PRIMARY KEY, replay_id INTEGER NOT NULL, slot_order INTEGER NOT NULL,
                joker_name TEXT NOT NULL, edition TEXT, enhancement TEXT, notes TEXT,
                FOREIGN KEY (replay_id) REFERENCES replay(id))"#,
        )
        .map_err(|e| format!("replay_joker_config table: {e}"))?;
        conn.execute_batch(
            r#"CREATE TABLE IF NOT EXISTS replay_hand_levels (
                id INTEGER PRIMARY KEY, replay_id INTEGER NOT NULL, hand_type TEXT NOT NULL,
                level INTEGER NOT NULL, chips INTEGER, mult INTEGER,
                FOREIGN KEY (replay_id) REFERENCES replay(id))"#,
        )
        .map_err(|e| format!("replay_hand_levels table: {e}"))?;
        conn.execute_batch(
            r#"CREATE TABLE IF NOT EXISTS replay_voucher (
                id INTEGER PRIMARY KEY, replay_id INTEGER NOT NULL, voucher_name TEXT NOT NULL,
                slot_order INTEGER, FOREIGN KEY (replay_id) REFERENCES replay(id))"#,
        )
        .map_err(|e| format!("replay_voucher table: {e}"))?;
        conn.execute_batch(
            r#"CREATE TABLE IF NOT EXISTS replay_economy (
                id INTEGER PRIMARY KEY, replay_id INTEGER NOT NULL, dollars_start INTEGER,
                dollars_end INTEGER, shop_items_bought TEXT, shop_items_skipped TEXT,
                FOREIGN KEY (replay_id) REFERENCES replay(id))"#,
        )
        .map_err(|e| format!("replay_economy table: {e}"))?;
        conn.execute_batch(
            r#"CREATE TABLE IF NOT EXISTS replay_tags (
                id INTEGER PRIMARY KEY, replay_id INTEGER NOT NULL, tag_name TEXT NOT NULL,
                source TEXT, FOREIGN KEY (replay_id) REFERENCES replay(id))"#,
        )
        .map_err(|e| format!("replay_tags table: {e}"))?;
        Ok(())
    }

    pub fn query_replays(
        &self,
        seed: Option<&str>,
        ante: Option<i64>,
        stake: Option<i64>,
        blind: Option<&str>,
        outcome: Option<&str>,
        json_mode: bool,
    ) -> Result<Value, String> {
        let conn = self.open()?;
        self.init_db(&conn)?;
        let mut query = "SELECT id, seed, ante, stake, blind_key, outcome, chips_required, max_chips_gained FROM replay WHERE 1=1".to_string();
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(s) = seed {
            query.push_str(" AND seed = ?");
            params.push(Box::new(s));
        }
        if let Some(a) = ante {
            query.push_str(" AND ante = ?");
            params.push(Box::new(a));
        }
        if let Some(s) = stake {
            query.push_str(" AND stake = ?");
            params.push(Box::new(s));
        }
        if let Some(b) = blind {
            query.push_str(" AND blind_key LIKE ?");
            let p = format!("%{}%", b);
            params.push(Box::new(p));
        }
        if let Some(o) = outcome {
            query.push_str(" AND outcome = ?");
            params.push(Box::new(o));
        }

        let mut stmt = conn.prepare(&query).map_err(|e| format!("prepare: {e}"))?;
        let params_ref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let replays: Vec<(
            i64,
            String,
            i64,
            i64,
            String,
            String,
            Option<i64>,
            Option<i64>,
        )> = stmt
            .query_map(
                params_ref.as_slice(),
                |row| -> Result<
                    (
                        i64,
                        String,
                        i64,
                        i64,
                        String,
                        String,
                        Option<i64>,
                        Option<i64>,
                    ),
                    rusqlite::Error,
                > {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                },
            )
            .map_err(|e| format!("query_map: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect: {e}"))?;

        if replays.is_empty() {
            return Ok(json!([]));
        }

        if json_mode {
            let results: Vec<Value> = replays
                .iter()
                .map(|(rid, _, _, _, _, _, _, _)| *rid)
                .filter_map(|rid| self.load_replay_detail(&conn, rid).ok())
                .collect();
            Ok(json!(results))
        } else {
            let mut output = String::new();
            for replay in &replays {
                output.push_str(&self.format_replay_text(&conn, replay));
            }
            Ok(json!({"text": output}))
        }
    }

    fn load_replay_detail(&self, conn: &Connection, replay_id: i64) -> Result<Value, String> {
        let mut jokers = Vec::new();
        let mut stmt = conn.prepare("SELECT slot_order, joker_name, edition, enhancement, notes FROM replay_joker_config WHERE replay_id=? ORDER BY slot_order").map_err(|e| e.to_string())?;
        for row in stmt
            .query_map(
                [replay_id],
                |row| -> Result<
                    (i64, String, Option<String>, Option<String>, Option<String>),
                    rusqlite::Error,
                > {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
        {
            let (slot, name, edition, enh, notes) = row;
            jokers.push(json!({"slot_order": slot, "joker_name": name, "edition": edition, "enhancement": enh, "notes": notes}));
        }

        let mut vouchers = Vec::new();
        let mut stmt = conn.prepare("SELECT slot_order, voucher_name FROM replay_voucher WHERE replay_id=? ORDER BY slot_order").map_err(|e| e.to_string())?;
        for row in stmt
            .query_map(
                [replay_id],
                |row| -> Result<(i64, String), rusqlite::Error> { Ok((row.get(0)?, row.get(1)?)) },
            )
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
        {
            let (slot, name) = row;
            vouchers.push(json!({"slot_order": slot, "voucher_name": name}));
        }

        let mut hand_levels = Vec::new();
        let mut stmt = conn.prepare("SELECT hand_type, level, chips, mult FROM replay_hand_levels WHERE replay_id=? ORDER BY hand_type").map_err(|e| e.to_string())?;
        for row in stmt
            .query_map(
                [replay_id],
                |row| -> Result<(String, i64, Option<i64>, Option<i64>), rusqlite::Error> {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                },
            )
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
        {
            let (ht, lvl, chips, mult) = row;
            hand_levels.push(json!({"hand_type": ht, "level": lvl, "chips": chips, "mult": mult}));
        }

        let mut steps = Vec::new();
        let mut stmt = conn.prepare("SELECT step_order, action_type, details, rationale, hand_type, cards_held, cards_discarded, discard_count, final_cards, base_chips, base_mult, final_score, consumable_name, consumable_target_hand, notes FROM replay_step WHERE replay_id=? ORDER BY step_order").map_err(|e| e.to_string())?;
        for row in stmt
            .query_map(
                [replay_id],
                |row| -> Result<
                    (
                        i64,
                        String,
                        String,
                        Option<String>,
                        String,
                        Option<String>,
                        Option<String>,
                        i64,
                        Option<String>,
                        Option<i64>,
                        Option<i64>,
                        Option<i64>,
                        Option<String>,
                        Option<String>,
                        Option<String>,
                    ),
                    rusqlite::Error,
                > {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                        row.get(11)?,
                        row.get(12)?,
                        row.get(13)?,
                        row.get(14)?,
                    ))
                },
            )
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
        {
            let (so, at, det, rat, ht, ch, cd, dc, fc, bc, bm, fs, cn, th, nt) = row;
            steps.push(json!({"step_order": so, "action_type": at, "details": det, "rationale": rat, "hand_type": ht, "cards_held": ch, "cards_discarded": cd, "discard_count": dc, "final_cards": fc, "base_chips": bc, "base_mult": bm, "final_score": fs, "consumable_name": cn, "consumable_target_hand": th, "notes": nt}));
        }

        let mut economy = None;
        let eco = conn.query_row(
            "SELECT dollars_start, dollars_end, shop_items_bought, shop_items_skipped FROM replay_economy WHERE replay_id=?",
            [replay_id],
            |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?))
        ).ok();
        if let Some((ds, de, sb, ss)) = eco {
            if ds.is_some() || de.is_some() {
                economy = Some(
                    json!({"dollars_start": ds, "dollars_end": de, "shop_items_bought": sb, "shop_items_skipped": ss}),
                );
            }
        }

        let mut tags = Vec::new();
        let mut stmt = conn
            .prepare("SELECT tag_name FROM replay_tags WHERE replay_id=?")
            .map_err(|e| e.to_string())?;
        for row in stmt
            .query_map([replay_id], |row| -> Result<String, rusqlite::Error> {
                row.get(0)
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
        {
            tags.push(json!(row));
        }

        Ok(
            json!({"jokers": jokers, "vouchers": vouchers, "hand_levels": hand_levels, "steps": steps, "economy": economy, "tags": tags}),
        )
    }

    fn format_replay_text(
        &self,
        conn: &Connection,
        replay: &(
            i64,
            String,
            i64,
            i64,
            String,
            String,
            Option<i64>,
            Option<i64>,
        ),
    ) -> String {
        let (rid, seed, ante, stake, blind, outcome, creq, cgained) = replay;
        let mut output = String::new();
        output.push_str(&"=".repeat(70));
        output.push('\n');
        if outcome == CLEAR {
            output.push_str(&format!(
                "REPLAY #{}: {} (Ante {}, Stake {}) - CLEARED",
                rid, blind, ante, stake
            ));
        } else {
            output.push_str(&format!(
                "REPLAY #{}: {} (Ante {}, Stake {}) - FAILED",
                rid, blind, ante, stake
            ));
        }
        output.push('\n');
        output.push_str(&format!("  Seed: {}", seed));
        output.push('\n');
        if outcome == CLEAR {
            if let Some(cr) = creq {
                output.push_str(&format!(
                    "  Required: {} chips | Achieved: {} chips",
                    cr,
                    cgained.unwrap_or(0)
                ));
                output.push('\n');
            }
        }
        let stmt = conn.prepare("SELECT slot_order, joker_name, edition, enhancement, notes FROM replay_joker_config WHERE replay_id=? ORDER BY slot_order").ok();
        if let Some(mut stmt) = stmt {
            let _ = stmt
                .query_row([*rid], |r| {
                    let sord: i64 = r.get(0)?;
                    let jname: String = r.get(1)?;
                    let edition: Option<String> = r.get(2)?;
                    let enh: Option<String> = r.get(3)?;
                    let notes: Option<String> = r.get(4)?;
                    let mut line = format!("    Slot {}: {}", sord, jname);
                    if let Some(e) = edition {
                        line.push_str(&format!(" {}", e));
                    }
                    if let Some(h) = enh {
                        line.push_str(&format!(" {}", h));
                    }
                    if let Some(n) = notes {
                        line.push_str(&format!(" | {}", n));
                    }
                    output.push_str(&line);
                    output.push('\n');
                    Ok(())
                })
                .ok();
        }
        let stmt = conn.prepare("SELECT step_order, action_type, details, rationale FROM replay_step WHERE replay_id=? ORDER BY step_order").ok();
        if let Some(mut stmt) = stmt {
            let _ = stmt
                .query_row([*rid], |r| {
                    let so: i64 = r.get(0)?;
                    let at: String = r.get(1)?;
                    let det: String = r.get(2)?;
                    let rat: Option<String> = r.get(3)?;
                    let line = format!("    Step {}: [{}] {}", so, at, det);
                    output.push_str(&line);
                    output.push('\n');
                    if let Some(r) = rat {
                        output.push_str(&format!("      Rationale: {}", r));
                        output.push('\n');
                    }
                    Ok(())
                })
                .ok();
        }
        output
    }

    pub fn log_clear(
        &self,
        seed: &str,
        ante: i64,
        stake: i64,
        blind_key: &str,
        jokers: &[(i64, &str, Option<&str>, Option<&str>, Option<&str>)],
        vouchers: &[&str],
        hand_levels: &[(&str, i64, Option<i64>, Option<i64>)],
        dollars_start: Option<i64>,
        dollars_end: Option<i64>,
        shop_bought: &str,
        shop_skipped: &str,
        steps: &[(
            String,
            String,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
            i64,
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<String>,
        )],
        tags: &[&str],
        _notes: &str,
    ) -> Result<i64, String> {
        let mut conn = self.open()?;
        self.init_db(&conn)?;
        let tx = conn
            .transaction()
            .map_err(|e| format!("transaction: {e}"))?;
        tx.execute(
            "INSERT INTO replay (seed, ante, stake, blind_key, outcome) VALUES (?,?,?,?,?)",
            rusqlite::params![seed, ante, stake, blind_key, CLEAR],
        )
        .map_err(|e| format!("insert replay: {e}"))?;
        let rid = tx.last_insert_rowid();
        for (idx, (action, details, rationale, ht, ch, cd, dc, fc, bc, bm, fs, cn, th, nt)) in
            steps.iter().enumerate()
        {
            tx.execute("INSERT INTO replay_step (replay_id, step_order, action_type, details, rationale, hand_type, cards_held, cards_discarded, discard_count, final_cards, base_chips, base_mult, final_score, consumable_name, consumable_target_hand, notes) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                rusqlite::params![rid, idx + 1, action, details, rationale, ht, ch, cd, dc, fc, bc, bm, fs, cn, th, nt])
                .map_err(|e| format!("insert step: {e}"))?;
        }
        for (slot, name, edition, enh, notes) in jokers {
            tx.execute("INSERT INTO replay_joker_config (replay_id, slot_order, joker_name, edition, enhancement, notes) VALUES (?,?,?,?,?,?)",
                rusqlite::params![rid, slot, name, edition, enh, notes])
                .map_err(|e| format!("insert joker: {e}"))?;
        }
        for (idx, vname) in vouchers.iter().enumerate() {
            tx.execute(
                "INSERT INTO replay_voucher (replay_id, slot_order, voucher_name) VALUES (?,?,?)",
                rusqlite::params![rid, idx + 1, vname],
            )
            .map_err(|e| format!("insert voucher: {e}"))?;
        }
        for (hname, lvl, chips, mult) in hand_levels {
            tx.execute("INSERT INTO replay_hand_levels (replay_id, hand_type, level, chips, mult) VALUES (?,?,?,?,?)",
                rusqlite::params![rid, hname, lvl, chips, mult])
                .map_err(|e| format!("insert hand_level: {e}"))?;
        }
        if dollars_start.is_some() || dollars_end.is_some() {
            tx.execute("INSERT INTO replay_economy (replay_id, dollars_start, dollars_end, shop_items_bought, shop_items_skipped) VALUES (?,?,?,?,?)",
                rusqlite::params![rid, dollars_start, dollars_end, shop_bought, shop_skipped])
                .map_err(|e| format!("insert economy: {e}"))?;
        }
        for tag in tags {
            tx.execute(
                "INSERT INTO replay_tags (replay_id, tag_name, source) VALUES (?,?,?)",
                rusqlite::params![rid, tag, "mcp"],
            )
            .map_err(|e| format!("insert tag: {e}"))?;
        }
        tx.commit().map_err(|e| format!("commit: {e}"))?;
        Ok(rid)
    }

    pub fn log_fail(
        &self,
        seed: &str,
        ante: i64,
        stake: i64,
        blind_key: &str,
    ) -> Result<i64, String> {
        let conn = self.open()?;
        self.init_db(&conn)?;
        conn.execute(
            "INSERT INTO replay (seed, ante, stake, blind_key, outcome) VALUES (?,?,?,?,?)",
            rusqlite::params![seed, ante, stake, blind_key, FAIL],
        )
        .map_err(|e| format!("insert replay: {e}"))?;
        Ok(conn.last_insert_rowid())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_db() -> (ReplayDB, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let db = ReplayDB::new(dir.path());
        (db, dir)
    }

    #[test]
    fn test_log_clear_and_query() {
        let (db, _dir) = make_db();
        let _rid = db
            .log_clear(
                "2K9H9HN",
                3,
                1,
                "S_1",
                &[],
                &[],
                &[],
                None,
                None,
                "",
                "",
                &[],
                &[],
                "",
            )
            .unwrap();
        let result = db
            .query_replays(Some("2K9H9HN"), None, None, None, None, true)
            .unwrap();
        assert!(result.is_array());
        assert!(!result.as_array().unwrap().is_empty());
    }

    #[test]
    fn test_log_clear_with_full_data() {
        let (db, _dir) = make_db();
        let jokers: Vec<(i64, &str, Option<&str>, Option<&str>, Option<&str>)> =
            vec![(0i64, "Joker", Some("Sealed"), Some("Bonus"), Some("note"))];
        let vouchers: Vec<&str> = vec!["Double Credit"];
        let hand_levels: Vec<(&str, i64, Option<i64>, Option<i64>)> =
            vec![("Flush", 3, Some(20), Some(5))];
        let steps: Vec<(
            String,
            String,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
            i64,
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = vec![(
            "play".to_string(),
            "played flush".to_string(),
            Some("good hand".to_string()),
            "Flush".to_string(),
            None,
            None,
            0,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )];
        let rid = db
            .log_clear(
                "2K9H9HN",
                3,
                1,
                "S_1",
                &jokers,
                &vouchers,
                &hand_levels,
                Some(10),
                Some(15),
                "Joker",
                "",
                &steps,
                &[],
                "",
            )
            .unwrap();
        assert!(rid > 0);
        let conn = db.open().unwrap();
        conn.execute(
            "UPDATE replay SET chips_required=100, max_chips_gained=120 WHERE id=?",
            [rid],
        )
        .unwrap();
        let result = db
            .query_replays(Some("2K9H9HN"), None, None, None, None, true)
            .unwrap();
        assert!(!result.as_array().unwrap().is_empty());
        let text = db
            .query_replays(None, None, None, None, None, false)
            .unwrap()["text"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(text.contains("Slot 0: Joker"));
        assert!(text.contains("Step 1: [play]"));
        assert!(text.contains("Rationale: good hand"));
    }

    #[test]
    fn test_log_fail() {
        let (db, _dir) = make_db();
        let rid = db.log_fail("2K9H9HN", 3, 1, "S_1").unwrap();
        assert!(rid > 0);
        let result = db
            .query_replays(None, None, None, None, Some("fail"), true)
            .unwrap();
        assert!(!result.as_array().unwrap().is_empty());
        let text = db
            .query_replays(None, None, None, None, Some("fail"), false)
            .unwrap();
        assert!(text["text"].as_str().unwrap().contains("FAILED"));
    }

    #[test]
    fn test_query_replays_empty() {
        let (db, _dir) = make_db();
        let result = db
            .query_replays(Some("NONEXISTENT"), None, None, None, None, true)
            .unwrap();
        assert!(result.is_array());
        assert_eq!(result.as_array().unwrap().len(), 0);
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let replay = (
            1,
            "seed".into(),
            1,
            1,
            "Small".into(),
            FAIL.into(),
            None,
            None,
        );
        let text = db.format_replay_text(&conn, &replay);
        assert!(text.contains("FAILED"));
    }

    #[test]
    fn test_query_replays_by_ante() {
        let (db, _dir) = make_db();
        db.log_clear(
            "2K9H9HN",
            3,
            1,
            "S_1",
            &[],
            &[],
            &[],
            None,
            None,
            "",
            "",
            &[],
            &[],
            "",
        )
        .unwrap();
        let result = db
            .query_replays(None, Some(3), None, None, None, true)
            .unwrap();
        assert!(!result.as_array().unwrap().is_empty());
    }

    #[test]
    fn test_query_replays_by_stake() {
        let (db, _dir) = make_db();
        db.log_clear(
            "2K9H9HN",
            3,
            1,
            "S_1",
            &[],
            &[],
            &[],
            None,
            None,
            "",
            "",
            &[],
            &[],
            "",
        )
        .unwrap();
        let result = db
            .query_replays(None, None, Some(1), None, None, true)
            .unwrap();
        assert!(!result.as_array().unwrap().is_empty());
    }

    #[test]
    fn test_query_replays_by_blind() {
        let (db, _dir) = make_db();
        db.log_clear(
            "2K9H9HN",
            3,
            1,
            "S_1",
            &[],
            &[],
            &[],
            None,
            None,
            "",
            "",
            &[],
            &[],
            "",
        )
        .unwrap();
        let result = db
            .query_replays(None, None, None, Some("S_1"), None, true)
            .unwrap();
        assert!(!result.as_array().unwrap().is_empty());
    }

    #[test]
    fn test_query_replays_text_mode() {
        let (db, _dir) = make_db();
        db.log_clear(
            "2K9H9HN",
            3,
            1,
            "S_1",
            &[],
            &[],
            &[],
            None,
            None,
            "",
            "",
            &[],
            &[],
            "",
        )
        .unwrap();
        let result = db
            .query_replays(None, None, None, None, None, false)
            .unwrap();
        assert!(result["text"].as_str().unwrap().contains("REPLAY"));
    }

    #[test]
    fn test_log_clear_with_economy() {
        let (db, _dir) = make_db();
        let steps: Vec<(
            String,
            String,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
            i64,
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = vec![(
            "play".to_string(),
            "played".to_string(),
            None,
            "High Card".to_string(),
            None,
            None,
            0,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )];
        let rid = db
            .log_clear(
                "2K9H9HN",
                3,
                1,
                "S_1",
                &[],
                &[],
                &[],
                Some(10),
                Some(15),
                "bought",
                "skipped",
                &steps,
                &[],
                "",
            )
            .unwrap();
        assert!(rid > 0);
        let result = db
            .query_replays(Some("2K9H9HN"), None, None, None, None, true)
            .unwrap();
        assert!(!result.as_array().unwrap().is_empty());
        let detail = result.as_array().unwrap()[0].as_object().unwrap();
        assert!(detail.contains_key("economy"));
    }

    #[test]
    fn test_log_clear_with_tags() {
        let (db, _dir) = make_db();
        let steps: Vec<(
            String,
            String,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
            i64,
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = vec![(
            "play".to_string(),
            "played".to_string(),
            None,
            "High Card".to_string(),
            None,
            None,
            0,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )];
        let rid = db
            .log_clear(
                "2K9H9HN",
                3,
                1,
                "S_1",
                &[],
                &[],
                &[],
                None,
                None,
                "",
                "",
                &steps,
                &["tag1", "tag2"],
                "",
            )
            .unwrap();
        let details = db
            .query_replays(None, None, None, None, None, true)
            .unwrap();
        assert_eq!(details[0]["tags"][0], "tag1");
        assert!(rid > 0);
    }

    #[test]
    fn test_multiple_replays() {
        let (db, _dir) = make_db();
        db.log_clear(
            "2K9H9HN",
            3,
            1,
            "S_1",
            &[],
            &[],
            &[],
            None,
            None,
            "",
            "",
            &[],
            &[],
            "",
        )
        .unwrap();
        db.log_clear(
            "2K9H9HN",
            4,
            2,
            "B_1",
            &[],
            &[],
            &[],
            None,
            None,
            "",
            "",
            &[],
            &[],
            "",
        )
        .unwrap();
        let result = db
            .query_replays(Some("2K9H9HN"), None, None, None, None, true)
            .unwrap();
        assert_eq!(result.as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_query_with_seed_and_blind() {
        let (db, _dir) = make_db();
        db.log_clear(
            "2K9H9HN",
            3,
            1,
            "S_1",
            &[],
            &[],
            &[],
            None,
            None,
            "",
            "",
            &[],
            &[],
            "",
        )
        .unwrap();
        let result = db
            .query_replays(Some("2K9H9HN"), None, None, Some("S_1"), None, true)
            .unwrap();
        assert!(!result.as_array().unwrap().is_empty());
    }

    #[test]
    fn test_query_with_seed_and_outcome() {
        let (db, _dir) = make_db();
        db.log_clear(
            "2K9H9HN",
            3,
            1,
            "S_1",
            &[],
            &[],
            &[],
            None,
            None,
            "",
            "",
            &[],
            &[],
            "",
        )
        .unwrap();
        let result = db
            .query_replays(Some("2K9H9HN"), None, None, None, Some("clear"), true)
            .unwrap();
        assert!(!result.as_array().unwrap().is_empty());
    }

    #[test]
    fn database_open_errors_are_reported() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("agent/replays.db")).unwrap();
        let db = ReplayDB::new(dir.path());
        assert!(
            db.query_replays(None, None, None, None, None, true)
                .is_err()
        );
        assert!(db.log_fail("2K9H9HN", 1, 1, "S_1").is_err());
        assert!(
            db.log_clear(
                "2K9H9HN",
                1,
                1,
                "S_1",
                &[],
                &[],
                &[],
                None,
                None,
                "",
                "",
                &[],
                &[],
                ""
            )
            .is_err()
        );
        let parent_file = dir.path().join("parent-file");
        std::fs::write(&parent_file, b"not a directory").unwrap();
        let db = ReplayDB::new(&parent_file);
        assert!(
            db.query_replays(None, None, None, None, None, true)
                .is_err()
        );
    }

    #[test]
    fn malformed_schema_errors_are_reported() {
        let dir = tempdir().unwrap();
        let db = ReplayDB::new(dir.path());
        let path = dir.path().join("agent/replays.db");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE replay (wrong TEXT); CREATE TABLE replay_step (wrong TEXT); CREATE TABLE replay_joker_config (wrong TEXT); CREATE TABLE replay_hand_levels (wrong TEXT); CREATE TABLE replay_voucher (wrong TEXT); CREATE TABLE replay_economy (wrong TEXT); CREATE TABLE replay_tags (wrong TEXT);",
        )
        .unwrap();
        drop(conn);
        assert!(
            db.query_replays(None, None, None, None, None, true)
                .is_err()
        );
        assert!(db.log_fail("2K9H9HN", 1, 1, "S_1").is_err());
        assert!(
            db.log_clear(
                "2K9H9HN",
                1,
                1,
                "S_1",
                &[],
                &[],
                &[],
                None,
                None,
                "",
                "",
                &[],
                &[],
                ""
            )
            .is_err()
        );
    }

    #[test]
    fn malformed_replay_rows_are_rejected_without_panicking() {
        let dir = tempdir().unwrap();
        let db = ReplayDB::new(dir.path());
        let conn = db.open().unwrap();
        db.init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO replay(id,seed,ante,stake,blind_key,outcome) VALUES(1,X'00',1,1,'Small','clear')",
            [],
        )
        .unwrap();
        drop(conn);
        assert!(
            db.query_replays(None, None, None, None, None, true)
                .is_err()
        );
    }

    #[test]
    fn replay_detail_handles_missing_tables_and_bad_rows() {
        let (db, _dir) = make_db();
        for table in [
            "replay_joker_config",
            "replay_voucher",
            "replay_hand_levels",
            "replay_step",
            "replay_tags",
        ] {
            let conn = Connection::open_in_memory().unwrap();
            db.init_db(&conn).unwrap();
            conn.execute_batch(&format!("DROP TABLE {table}")).unwrap();
            assert!(db.load_replay_detail(&conn, 1).is_err(), "{table}");
        }

        let cases = [
            "INSERT INTO replay_joker_config (replay_id, slot_order, joker_name) VALUES (1, X'00', 'J')",
            "INSERT INTO replay_voucher (replay_id, slot_order, voucher_name) VALUES (1, X'00', 'V')",
            "INSERT INTO replay_hand_levels (replay_id, hand_type, level) VALUES (1, 'Flush', X'00')",
            "INSERT INTO replay_step (replay_id, step_order, action_type, details, hand_type, discard_count) VALUES (1, X'00', 'play', 'd', 'Flush', 0)",
            "INSERT INTO replay_tags (replay_id, tag_name) VALUES (1, X'00')",
        ];
        for insert in cases {
            let conn = Connection::open_in_memory().unwrap();
            db.init_db(&conn).unwrap();
            conn.execute("INSERT INTO replay (id, seed, ante, stake, blind_key, outcome) VALUES (1, 'seed', 1, 1, 'Small', 'clear')", []).unwrap();
            conn.execute(insert, []).unwrap();
            assert!(db.load_replay_detail(&conn, 1).is_err(), "{insert}");
        }

        for column in [
            "slot_order",
            "joker_name",
            "edition",
            "enhancement",
            "notes",
        ] {
            let conn = Connection::open_in_memory().unwrap();
            db.init_db(&conn).unwrap();
            conn.execute("INSERT INTO replay (id, seed, ante, stake, blind_key, outcome) VALUES (1, 'seed', 1, 1, 'Small', 'clear')", []).unwrap();
            conn.execute("INSERT INTO replay_joker_config (replay_id, slot_order, joker_name) VALUES (1, 1, 'J')", []).unwrap();
            conn.execute(
                &format!("UPDATE replay_joker_config SET {column}=X'00' WHERE replay_id=1"),
                [],
            )
            .unwrap();
            assert!(db.load_replay_detail(&conn, 1).is_err(), "{column}");
        }
        for column in ["slot_order", "voucher_name"] {
            let conn = Connection::open_in_memory().unwrap();
            db.init_db(&conn).unwrap();
            conn.execute("INSERT INTO replay (id, seed, ante, stake, blind_key, outcome) VALUES (1, 'seed', 1, 1, 'Small', 'clear')", []).unwrap();
            conn.execute("INSERT INTO replay_voucher (replay_id, slot_order, voucher_name) VALUES (1, 1, 'V')", []).unwrap();
            conn.execute(
                &format!("UPDATE replay_voucher SET {column}=X'00' WHERE replay_id=1"),
                [],
            )
            .unwrap();
            assert!(db.load_replay_detail(&conn, 1).is_err(), "{column}");
        }
        for column in ["hand_type", "level", "chips", "mult"] {
            let conn = Connection::open_in_memory().unwrap();
            db.init_db(&conn).unwrap();
            conn.execute("INSERT INTO replay (id, seed, ante, stake, blind_key, outcome) VALUES (1, 'seed', 1, 1, 'Small', 'clear')", []).unwrap();
            conn.execute("INSERT INTO replay_hand_levels (replay_id, hand_type, level) VALUES (1, 'Flush', 1)", []).unwrap();
            conn.execute(
                &format!("UPDATE replay_hand_levels SET {column}=X'00' WHERE replay_id=1"),
                [],
            )
            .unwrap();
            assert!(db.load_replay_detail(&conn, 1).is_err(), "{column}");
        }
    }

    #[test]
    fn replay_text_handles_optional_and_malformed_detail_rows() {
        let (db, _dir) = make_db();
        let replay = (
            1,
            "seed".to_string(),
            1,
            1,
            "Small".to_string(),
            CLEAR.to_string(),
            Some(10),
            Some(12),
        );
        let conn = Connection::open_in_memory().unwrap();
        db.init_db(&conn).unwrap();
        conn.execute("INSERT INTO replay (id, seed, ante, stake, blind_key, outcome) VALUES (1, 'seed', 1, 1, 'Small', 'clear')", []).unwrap();
        conn.execute("INSERT INTO replay_joker_config (replay_id, slot_order, joker_name, edition, enhancement, notes) VALUES (1, 1, 'J', 'Foil', 'Bonus', 'note')", []).unwrap();
        conn.execute("INSERT INTO replay_step (replay_id, step_order, action_type, details, rationale, hand_type, discard_count) VALUES (1, 1, 'play', 'details', 'why', 'Flush', 0)", []).unwrap();
        let text = db.format_replay_text(&conn, &replay);
        assert!(text.contains("Required: 10 chips | Achieved: 12 chips"));
        assert!(text.contains("Foil Bonus | note"));
        assert!(text.contains("Rationale: why"));

        let conn = Connection::open_in_memory().unwrap();
        db.init_db(&conn).unwrap();
        conn.execute("INSERT INTO replay (id, seed, ante, stake, blind_key, outcome) VALUES (1, 'seed', 1, 1, 'Small', 'clear')", []).unwrap();
        conn.execute("INSERT INTO replay_joker_config (replay_id, slot_order, joker_name) VALUES (1, X'00', 'J')", []).unwrap();
        conn.execute("INSERT INTO replay_step (replay_id, step_order, action_type, details, hand_type, discard_count) VALUES (1, X'00', 'play', 'details', 'Flush', 0)", []).unwrap();
        let _ = db.format_replay_text(&conn, &replay);
    }
}
