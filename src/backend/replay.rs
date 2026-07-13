use rusqlite::{Connection, OpenFlags, Row, TransactionBehavior};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

const CLEAR: &str = "clear";
const FAIL: &str = "fail";
pub const MAX_REPLAY_NOTE_BYTES: usize = 64 * 1024;

fn bounded_replay_note(note: Option<&String>) -> Option<String> {
    let note = note?;
    if note.len() <= MAX_REPLAY_NOTE_BYTES {
        return Some(note.clone());
    }
    Some(
        json!({
            "truncated": true,
            "bytes": note.len(),
        })
        .to_string(),
    )
}

type ReplayRow = (
    i64,
    String,
    i64,
    i64,
    String,
    String,
    Option<i64>,
    Option<i64>,
);
type JokerRow = (i64, String, Option<String>, Option<String>, Option<String>);
type VoucherRow = (i64, String);
type HandLevelRow = (String, i64, Option<i64>, Option<i64>);
type StepRow = (
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
);
type FormatStepRow = (i64, String, String, Option<String>);
type EconomyRow = (Option<i64>, Option<i64>, Option<String>, Option<String>);

fn read_replay_row(row: &Row<'_>) -> rusqlite::Result<ReplayRow> {
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
}

fn read_joker_row(row: &Row<'_>) -> rusqlite::Result<JokerRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

fn read_voucher_row(row: &Row<'_>) -> rusqlite::Result<VoucherRow> {
    Ok((row.get(0)?, row.get(1)?))
}

fn read_hand_level_row(row: &Row<'_>) -> rusqlite::Result<HandLevelRow> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn read_step_row(row: &Row<'_>) -> rusqlite::Result<StepRow> {
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
}

fn read_tag_row(row: &Row<'_>) -> rusqlite::Result<String> {
    row.get(0)
}

fn read_format_step_row(row: &Row<'_>) -> rusqlite::Result<FormatStepRow> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn read_economy_row(row: &Row<'_>) -> rusqlite::Result<EconomyRow> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn sql_context<T>(result: rusqlite::Result<T>, context: &str) -> Result<T, String> {
    match result {
        Ok(value) => Ok(value),
        Err(error) => Err(format!("{context}: {error}")),
    }
}

trait SqlContext<T> {
    fn context(self, context: &str) -> Result<T, String>;
}

impl<T> SqlContext<T> for rusqlite::Result<T> {
    fn context(self, context: &str) -> Result<T, String> {
        sql_context(self, context)
    }
}

fn begin_replay_transaction(conn: &mut Connection) -> Result<rusqlite::Transaction<'_>, String> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("transaction")?;
    Ok(tx)
}

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
            .expect("ReplayDB paths always include the agent directory");
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create replay DB directory: {e}"))?;
        Connection::open_with_flags(
            &self.db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .map_err(|e| format!("cannot open replay DB: {e}"))
    }

    fn init_db(&self, conn: &Connection) -> Result<(), String> {
        sql_context(conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS replay (
                id INTEGER PRIMARY KEY, seed TEXT NOT NULL, ante INTEGER NOT NULL,
                stake INTEGER NOT NULL, blind_key TEXT NOT NULL, outcome TEXT NOT NULL,
                chips_required INTEGER, max_chips_gained INTEGER,
                created_at TEXT DEFAULT (datetime('now')));
            CREATE TABLE IF NOT EXISTS replay_step (
                id INTEGER PRIMARY KEY, replay_id INTEGER NOT NULL, step_order INTEGER,
                action_type TEXT NOT NULL, details TEXT NOT NULL, rationale TEXT,
                hand_type TEXT, cards_held TEXT, cards_discarded TEXT, discard_count INTEGER DEFAULT 0,
                final_cards TEXT, base_chips INTEGER, base_mult INTEGER, final_score INTEGER,
                consumable_name TEXT, consumable_target_hand TEXT, notes TEXT,
                FOREIGN KEY (replay_id) REFERENCES replay(id));
            CREATE TABLE IF NOT EXISTS replay_joker_config (
                id INTEGER PRIMARY KEY, replay_id INTEGER NOT NULL, slot_order INTEGER NOT NULL,
                joker_name TEXT NOT NULL, edition TEXT, enhancement TEXT, notes TEXT,
                FOREIGN KEY (replay_id) REFERENCES replay(id));
            CREATE TABLE IF NOT EXISTS replay_hand_levels (
                id INTEGER PRIMARY KEY, replay_id INTEGER NOT NULL, hand_type TEXT NOT NULL,
                level INTEGER NOT NULL, chips INTEGER, mult INTEGER,
                FOREIGN KEY (replay_id) REFERENCES replay(id));
            CREATE TABLE IF NOT EXISTS replay_voucher (
                id INTEGER PRIMARY KEY, replay_id INTEGER NOT NULL, voucher_name TEXT NOT NULL,
                slot_order INTEGER, FOREIGN KEY (replay_id) REFERENCES replay(id));
            CREATE TABLE IF NOT EXISTS replay_economy (
                id INTEGER PRIMARY KEY, replay_id INTEGER NOT NULL, dollars_start INTEGER,
                dollars_end INTEGER, shop_items_bought TEXT, shop_items_skipped TEXT,
                FOREIGN KEY (replay_id) REFERENCES replay(id));
            CREATE TABLE IF NOT EXISTS replay_tags (
                id INTEGER PRIMARY KEY, replay_id INTEGER NOT NULL, tag_name TEXT NOT NULL,
                source TEXT, FOREIGN KEY (replay_id) REFERENCES replay(id));
            "#,
        ), "replay schema")?;
        for (table, required_column) in [
            ("replay", "seed"),
            ("replay_step", "action_type"),
            ("replay_joker_config", "joker_name"),
            ("replay_hand_levels", "hand_type"),
            ("replay_voucher", "voucher_name"),
            ("replay_economy", "dollars_start"),
            ("replay_tags", "tag_name"),
        ] {
            let present: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info(?) WHERE name=?",
                    rusqlite::params![table, required_column],
                    |row| row.get(0),
                )
                .expect("static replay schema validation query");
            if present == 0 {
                return Err(format!(
                    "replay schema missing {required_column} in {table}"
                ));
            }
        }
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
        query.push_str(" ORDER BY id DESC");

        let mut stmt = conn.prepare(&query).map_err(|e| format!("prepare: {e}"))?;
        let params_ref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let replay_rows = stmt
            .query_map(params_ref.as_slice(), read_replay_row)
            .expect("replay query parameters are statically typed");
        let replays: Vec<ReplayRow> = replay_rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect: {e}"))?;

        if replays.is_empty() {
            return Ok(json!([]));
        }

        if json_mode {
            let results: Result<Vec<Value>, String> = replays
                .iter()
                .map(
                    |(rid, seed, ante, stake, blind, outcome, chips_required, max_chips_gained)| {
                        let detail = self.load_replay_detail(&conn, *rid)?;
                        let mut object = detail
                            .as_object()
                            .cloned()
                            .ok_or_else(|| format!("replay {rid} detail is not an object"))?;
                        object.insert("replay_id".into(), json!(rid));
                        object.insert("seed".into(), json!(seed));
                        object.insert("ante".into(), json!(ante));
                        object.insert("stake".into(), json!(stake));
                        object.insert("blind_key".into(), json!(blind));
                        object.insert("outcome".into(), json!(outcome));
                        object.insert("chips_required".into(), json!(chips_required));
                        object.insert("max_chips_gained".into(), json!(max_chips_gained));
                        Ok(Value::Object(object))
                    },
                )
                .collect();
            Ok(json!(results?))
        } else {
            let mut output = String::new();
            for replay in &replays {
                output.push_str(&self.format_replay_text(&conn, replay));
            }
            Ok(json!({"text": output}))
        }
    }

    pub fn health(&self) -> Result<Value, String> {
        let bytes = fs::metadata(&self.db_path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let conn = self.open()?;
        self.init_db(&conn)?;
        let (replays, steps, max_notes): (i64, i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM replay), (SELECT COUNT(*) FROM replay_step), COALESCE((SELECT MAX(length(notes)) FROM replay_step), 0)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .context("replay health")?;
        Ok(json!({
            "database_bytes": bytes,
            "replays": replays,
            "replay_steps": steps,
            "max_step_notes_bytes": max_notes,
        }))
    }

    fn load_replay_detail(&self, conn: &Connection, replay_id: i64) -> Result<Value, String> {
        let mut jokers = Vec::new();
        let mut stmt = conn.prepare("SELECT slot_order, joker_name, edition, enhancement, notes FROM replay_joker_config WHERE replay_id=? ORDER BY slot_order").map_err(|e| e.to_string())?;
        let joker_rows = stmt
            .query_map([replay_id], read_joker_row)
            .expect("joker detail query parameters are statically typed");
        for row in joker_rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
        {
            let (slot, name, edition, enh, notes) = row;
            jokers.push(json!({"slot_order": slot, "joker_name": name, "edition": edition, "enhancement": enh, "notes": notes}));
        }

        let mut vouchers = Vec::new();
        let mut stmt = conn.prepare("SELECT slot_order, voucher_name FROM replay_voucher WHERE replay_id=? ORDER BY slot_order").map_err(|e| e.to_string())?;
        let voucher_rows = stmt
            .query_map([replay_id], read_voucher_row)
            .expect("voucher detail query parameters are statically typed");
        for row in voucher_rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
        {
            let (slot, name) = row;
            vouchers.push(json!({"slot_order": slot, "voucher_name": name}));
        }

        let mut hand_levels = Vec::new();
        let mut stmt = conn.prepare("SELECT hand_type, level, chips, mult FROM replay_hand_levels WHERE replay_id=? ORDER BY hand_type").map_err(|e| e.to_string())?;
        let hand_level_rows = stmt
            .query_map([replay_id], read_hand_level_row)
            .expect("hand-level query parameters are statically typed");
        for row in hand_level_rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
        {
            let (ht, lvl, chips, mult) = row;
            hand_levels.push(json!({"hand_type": ht, "level": lvl, "chips": chips, "mult": mult}));
        }

        let mut steps = Vec::new();
        let mut stmt = conn.prepare("SELECT step_order, action_type, details, rationale, hand_type, cards_held, cards_discarded, discard_count, final_cards, base_chips, base_mult, final_score, consumable_name, consumable_target_hand, notes FROM replay_step WHERE replay_id=? ORDER BY step_order").map_err(|e| e.to_string())?;
        let step_rows = stmt
            .query_map([replay_id], read_step_row)
            .expect("step detail query parameters are statically typed");
        for row in step_rows
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
            read_economy_row
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
        let tag_rows = stmt
            .query_map([replay_id], read_tag_row)
            .expect("tag detail query parameters are statically typed");
        for row in tag_rows
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
                    let (sord, jname, edition, enh, notes) = read_joker_row(r)?;
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
                    let (so, at, det, rat) = read_format_step_row(r)?;
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
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .expect("replay foreign-key pragma is static");
        let tx = begin_replay_transaction(&mut conn)?;
        self.init_db(&tx)?;
        tx.execute(
            "INSERT INTO replay (seed, ante, stake, blind_key, outcome) VALUES (?,?,?,?,?)",
            rusqlite::params![seed, ante, stake, blind_key, CLEAR],
        )
        .context("insert replay")?;
        let rid = tx.last_insert_rowid();
        for (idx, (action, details, rationale, ht, ch, cd, dc, fc, bc, bm, fs, cn, th, nt)) in
            steps.iter().enumerate()
        {
            let bounded_notes = bounded_replay_note(nt.as_ref());
            tx.execute("INSERT INTO replay_step (replay_id, step_order, action_type, details, rationale, hand_type, cards_held, cards_discarded, discard_count, final_cards, base_chips, base_mult, final_score, consumable_name, consumable_target_hand, notes) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                rusqlite::params![rid, idx + 1, action, details, rationale, ht, ch, cd, dc, fc, bc, bm, fs, cn, th, bounded_notes])
                .context("insert step")?;
        }
        for (slot, name, edition, enh, notes) in jokers {
            tx.execute("INSERT INTO replay_joker_config (replay_id, slot_order, joker_name, edition, enhancement, notes) VALUES (?,?,?,?,?,?)",
                rusqlite::params![rid, slot, name, edition, enh, notes])
                .context("insert joker")?;
        }
        for (idx, vname) in vouchers.iter().enumerate() {
            tx.execute(
                "INSERT INTO replay_voucher (replay_id, slot_order, voucher_name) VALUES (?,?,?)",
                rusqlite::params![rid, idx + 1, vname],
            )
            .context("insert voucher")?;
        }
        for (hname, lvl, chips, mult) in hand_levels {
            tx.execute("INSERT INTO replay_hand_levels (replay_id, hand_type, level, chips, mult) VALUES (?,?,?,?,?)",
                rusqlite::params![rid, hname, lvl, chips, mult])
                .context("insert hand_level")?;
        }
        if dollars_start.is_some() || dollars_end.is_some() {
            tx.execute("INSERT INTO replay_economy (replay_id, dollars_start, dollars_end, shop_items_bought, shop_items_skipped) VALUES (?,?,?,?,?)",
                rusqlite::params![rid, dollars_start, dollars_end, shop_bought, shop_skipped])
                .context("insert economy")?;
        }
        for tag in tags {
            tx.execute(
                "INSERT INTO replay_tags (replay_id, tag_name, source) VALUES (?,?,?)",
                rusqlite::params![rid, tag, "mcp"],
            )
            .context("insert tag")?;
        }
        tx.commit().context("commit")?;
        Ok(rid)
    }

    pub fn log_fail(
        &self,
        seed: &str,
        ante: i64,
        stake: i64,
        blind_key: &str,
    ) -> Result<i64, String> {
        self.log_fail_with_steps(seed, ante, stake, blind_key, &[])
    }

    pub fn log_fail_with_steps(
        &self,
        seed: &str,
        ante: i64,
        stake: i64,
        blind_key: &str,
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
    ) -> Result<i64, String> {
        let conn = self.open()?;
        self.init_db(&conn)?;
        conn.execute(
            "INSERT INTO replay (seed, ante, stake, blind_key, outcome) VALUES (?,?,?,?,?)",
            rusqlite::params![seed, ante, stake, blind_key, FAIL],
        )
        .context("insert replay")?;
        let rid = conn.last_insert_rowid();
        for (idx, (action, details, rationale, ht, ch, cd, dc, fc, bc, bm, fs, cn, th, nt)) in
            steps.iter().enumerate()
        {
            let bounded_notes = bounded_replay_note(nt.as_ref());
            conn.execute("INSERT INTO replay_step (replay_id, step_order, action_type, details, rationale, hand_type, cards_held, cards_discarded, discard_count, final_cards, base_chips, base_mult, final_score, consumable_name, consumable_target_hand, notes) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                rusqlite::params![rid, idx + 1, action, details, rationale, ht, ch, cd, dc, fc, bc, bm, fs, cn, th, bounded_notes])
                .context("insert failed replay step")?;
        }
        Ok(rid)
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
        assert_eq!(result[0]["replay_id"], rid);
        assert_eq!(result[0]["seed"], "2K9H9HN");
        assert_eq!(result[0]["outcome"], "fail");
        let text = db
            .query_replays(None, None, None, None, Some("fail"), false)
            .unwrap();
        assert!(text["text"].as_str().unwrap().contains("FAILED"));
    }

    #[test]
    fn oversized_replay_notes_are_reduced_to_bounded_summaries() {
        let (db, _dir) = make_db();
        let oversized = "x".repeat(MAX_REPLAY_NOTE_BYTES + 1);
        let steps = vec![(
            "play".to_string(),
            "details".to_string(),
            None,
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
            Some(oversized),
        )];
        db.log_fail_with_steps("2K9H9HN", 1, 1, "Small", &steps)
            .unwrap();
        let health = db.health().unwrap();
        assert!(health["max_step_notes_bytes"].as_i64().unwrap() <= 64);
        let conn = db.open().unwrap();
        let note: String = conn
            .query_row("SELECT notes FROM replay_step", [], |row| row.get(0))
            .unwrap();
        assert!(note.contains("truncated"));
    }

    #[test]
    fn query_replays_orders_latest_first_and_keeps_all_outcomes() {
        let (db, _dir) = make_db();
        let clear = db
            .log_clear(
                "2K9H9HN",
                1,
                1,
                "Small",
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
        let fail = db.log_fail("2K9H9HN", 1, 1, "Small").unwrap();
        let result = db
            .query_replays(Some("2K9H9HN"), Some(1), Some(1), None, None, true)
            .unwrap();
        assert_eq!(result[0]["replay_id"], fail);
        assert_eq!(result[1]["replay_id"], clear);
        assert_eq!(result[0]["outcome"], "fail");
        assert_eq!(result[1]["outcome"], "clear");
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

        let dir = tempdir().unwrap();
        let db = ReplayDB::new(dir.path());
        let conn = db.open().unwrap();
        db.init_db(&conn).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_replay_log BEFORE INSERT ON replay BEGIN SELECT RAISE(ABORT, 'blocked'); END;",
        )
        .unwrap();
        assert!(db.log_fail("seed", 1, 1, "Small").is_err());
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

        let dir = tempdir().unwrap();
        let db = ReplayDB::new(dir.path());
        let conn = db.open().unwrap();
        db.init_db(&conn).unwrap();
        conn.execute_batch("DROP TABLE replay; CREATE TABLE replay (seed TEXT NOT NULL);")
            .unwrap();
        assert!(
            db.query_replays(None, None, None, None, None, true)
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
    fn replay_row_decoders_cover_every_field_error() {
        assert!(sql_context::<()>(Ok(()), "ok").is_ok());
        assert!(sql_context::<()>(Err(rusqlite::Error::QueryReturnedNoRows), "bad").is_err());
        macro_rules! assert_reader_error {
            ($values:expr, $reader:path) => {{
                let conn = Connection::open_in_memory().unwrap();
                let mut stmt = conn
                    .prepare(&format!("SELECT {}", $values.join(",")))
                    .unwrap();
                assert!(stmt.query_row([], $reader).is_err());
            }};
        }

        let replay_values = [
            "1", "'seed'", "1", "1", "'Small'", "'clear'", "NULL", "NULL",
        ];
        for bad in 0..replay_values.len() {
            let mut values = replay_values.map(str::to_owned);
            values[bad] = "X'00'".into();
            assert_reader_error!(values, read_replay_row);
        }

        let joker_values = ["1", "'Joker'", "NULL", "NULL", "NULL"];
        for bad in 0..joker_values.len() {
            let mut values = joker_values.map(str::to_owned);
            values[bad] = "X'00'".into();
            assert_reader_error!(values, read_joker_row);
        }

        let voucher_values = ["1", "'Voucher'"];
        for bad in 0..voucher_values.len() {
            let mut values = voucher_values.map(str::to_owned);
            values[bad] = "X'00'".into();
            assert_reader_error!(values, read_voucher_row);
        }

        let hand_level_values = ["'Flush'", "1", "NULL", "NULL"];
        for bad in 0..hand_level_values.len() {
            let mut values = hand_level_values.map(str::to_owned);
            values[bad] = "X'00'".into();
            assert_reader_error!(values, read_hand_level_row);
        }

        let step_values = [
            "1",
            "'play'",
            "'details'",
            "NULL",
            "'Flush'",
            "NULL",
            "NULL",
            "0",
            "NULL",
            "NULL",
            "NULL",
            "NULL",
            "NULL",
            "NULL",
            "NULL",
        ];
        for bad in 0..step_values.len() {
            let mut values = step_values.map(str::to_owned);
            values[bad] = "X'00'".into();
            assert_reader_error!(values, read_step_row);
        }

        assert_reader_error!(vec!["X'00'".to_owned()], read_tag_row);

        let format_step_values = ["1", "'play'", "'details'", "NULL"];
        for bad in 0..format_step_values.len() {
            let mut values = format_step_values.map(str::to_owned);
            values[bad] = "X'00'".into();
            assert_reader_error!(values, read_format_step_row);
        }

        let economy_values = ["NULL", "NULL", "NULL", "NULL"];
        for bad in 0..economy_values.len() {
            let mut values = economy_values.map(str::to_owned);
            values[bad] = "X'00'".into();
            assert_reader_error!(values, read_economy_row);
        }
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
        conn.execute("INSERT INTO replay_joker_config (replay_id, slot_order, joker_name) VALUES (1, 1, 'J')", []).unwrap();
        conn.execute("INSERT INTO replay_step (replay_id, step_order, action_type, details, hand_type, discard_count) VALUES (1, 1, 'play', 'details', 'Flush', 0)", []).unwrap();
        let text = db.format_replay_text(&conn, &replay);
        assert!(text.contains("Slot 1: J"));
        assert!(!text.contains("Slot 1: J Foil"));
        assert!(!text.contains("Rationale:"));

        let conn = Connection::open_in_memory().unwrap();
        db.init_db(&conn).unwrap();
        conn.execute("INSERT INTO replay (id, seed, ante, stake, blind_key, outcome) VALUES (1, 'seed', 1, 1, 'Small', 'clear')", []).unwrap();
        conn.execute("INSERT INTO replay_joker_config (replay_id, slot_order, joker_name) VALUES (1, X'00', 'J')", []).unwrap();
        conn.execute("INSERT INTO replay_step (replay_id, step_order, action_type, details, hand_type, discard_count) VALUES (1, X'00', 'play', 'details', 'Flush', 0)", []).unwrap();
        let _ = db.format_replay_text(&conn, &replay);

        let conn = Connection::open_in_memory().unwrap();
        db.init_db(&conn).unwrap();
        conn.execute("INSERT INTO replay (id, seed, ante, stake, blind_key, outcome) VALUES (1, 'seed', 1, 1, 'Small', 'clear')", []).unwrap();
        conn.execute("INSERT INTO replay_economy (replay_id) VALUES (1)", [])
            .unwrap();
        let detail = db.load_replay_detail(&conn, 1).unwrap();
        assert!(detail["economy"].is_null());
        conn.execute(
            "UPDATE replay_economy SET dollars_start=10 WHERE replay_id=1",
            [],
        )
        .unwrap();
        let detail = db.load_replay_detail(&conn, 1).unwrap();
        assert_eq!(detail["economy"]["dollars_start"], 10);
        conn.execute(
            "UPDATE replay_economy SET dollars_start=X'00' WHERE replay_id=1",
            [],
        )
        .unwrap();
        assert!(db.load_replay_detail(&conn, 1).unwrap()["economy"].is_null());
    }

    #[test]
    fn replay_write_boundaries_return_sql_errors() {
        fn failing_db(table: &str) -> (tempfile::TempDir, ReplayDB) {
            let dir = tempdir().unwrap();
            let db = ReplayDB::new(dir.path());
            let conn = db.open().unwrap();
            db.init_db(&conn).unwrap();
            conn.execute_batch(&format!(
                "CREATE TRIGGER fail_{table}_insert BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT, 'blocked'); END;"
            ))
            .unwrap();
            (dir, db)
        }

        for table in [
            "replay",
            "replay_step",
            "replay_joker_config",
            "replay_voucher",
            "replay_hand_levels",
            "replay_economy",
            "replay_tags",
        ] {
            let (_dir, db) = failing_db(table);
            let jokers = vec![(0_i64, "J", None, None, None)];
            let vouchers = vec!["V"];
            let hand_levels = vec![("Flush", 1_i64, Some(1_i64), Some(1_i64))];
            let steps = vec![(
                "play".to_string(),
                "details".to_string(),
                None,
                "Flush".to_string(),
                None,
                None,
                0_i64,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )];
            let result = db.log_clear(
                "seed",
                1,
                1,
                "Small",
                &jokers,
                &vouchers,
                &hand_levels,
                Some(1),
                Some(2),
                "bought",
                "skipped",
                &steps,
                &["tag"],
                "",
            );
            assert!(result.is_err(), "{table}");
        }
    }

    #[test]
    fn replay_transaction_and_commit_errors_are_returned() {
        let dir = tempdir().unwrap();
        let db = ReplayDB::new(dir.path());
        let conn = db.open().unwrap();
        db.init_db(&conn).unwrap();
        conn.execute_batch(
            "CREATE TABLE deferred_replay_child (parent_id INTEGER REFERENCES replay(id) DEFERRABLE INITIALLY DEFERRED);
             CREATE TRIGGER invalid_replay_commit AFTER INSERT ON replay BEGIN
                 INSERT INTO deferred_replay_child(parent_id) VALUES(999);
             END;",
        )
        .unwrap();
        drop(conn);
        assert!(
            db.log_clear(
                "seed",
                1,
                1,
                "Small",
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
            .is_err()
        );

        let dir = tempdir().unwrap();
        let db = ReplayDB::new(dir.path());
        let conn = db.open().unwrap();
        db.init_db(&conn).unwrap();
        conn.execute_batch("BEGIN EXCLUSIVE").unwrap();
        assert!(
            db.query_replays(None, None, None, None, None, true)
                .is_err()
        );
        assert!(db.log_fail("seed", 1, 1, "Small").is_err());
        let locked_error = db
            .log_clear(
                "seed",
                1,
                1,
                "Small",
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
            .unwrap_err();
        assert!(locked_error.contains("locked") || locked_error.contains("transaction"));

        let dir = tempdir().unwrap();
        let db = ReplayDB::new(dir.path());
        let mut conn = db.open().unwrap();
        db.init_db(&conn).unwrap();
        conn.execute_batch("BEGIN").unwrap();
        assert!(begin_replay_transaction(&mut conn).is_err());
    }
}
