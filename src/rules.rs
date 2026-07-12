use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Map, Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

const TABLES: &[(&str, &str)] = &[
    ("self.P_CENTERS =", "center"),
    ("self.P_BLINDS =", "blind"),
    ("self.P_TAGS =", "tag"),
    ("self.P_SEALS =", "seal"),
    ("self.P_STAKES =", "stake"),
    ("self.P_CARDS =", "playing_card"),
];

pub fn default_db_path(root: &Path) -> PathBuf {
    root.join("data").join("balatro.sqlite")
}
pub fn default_source_path(root: &Path) -> PathBuf {
    root.join("rules").join("balatro_src")
}

pub struct RulesDb {
    path: PathBuf,
}

impl RulesDb {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
    fn open(&self) -> Result<Connection, String> {
        if !self.path.exists() {
            return Err(format!("database missing: {}", self.path.display()));
        }
        Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("database open failed: {e}"))
    }

    pub fn lookup(&self, kind: &str, name: &str, options: &LookupOptions) -> Result<Value, String> {
        let db = self.open()?;
        lookup(&db, kind, name, options)
    }
    pub fn list(&self, kind: Option<&str>) -> Result<Value, String> {
        let db = self.open()?;
        let mut rows = if let Some(kind) = kind {
            let mut stmt = db.prepare("SELECT key, set_name AS type, name FROM entities WHERE kind = ?1 OR set_name = ?1 COLLATE NOCASE ORDER BY name").map_err(sql)?;
            stmt.query_map([kind], |r| Ok(json!({"key": r.get::<_, String>(0)?, "type": r.get::<_, Option<String>>(1)?, "name": r.get::<_, String>(2)?}))).map_err(sql)?.collect::<Result<Vec<_>, _>>().map_err(sql)?
        } else {
            let mut stmt = db
                .prepare("SELECT key, set_name AS type, name FROM entities ORDER BY set_name, name")
                .map_err(sql)?;
            stmt.query_map([], |r| Ok(json!({"key": r.get::<_, String>(0)?, "type": r.get::<_, Option<String>>(1)?, "name": r.get::<_, String>(2)?}))).map_err(sql)?.collect::<Result<Vec<_>, _>>().map_err(sql)?
        };
        Ok(json!({"count": rows.len(), "entities": rows.drain(..).collect::<Vec<_>>() }))
    }
    pub fn stats(&self) -> Result<Value, String> {
        let db = self.open()?;
        let mut metadata = Map::new();
        let mut stmt = db.prepare("SELECT key, value FROM metadata").map_err(sql)?;
        for row in stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(sql)?
        {
            let (key, value) = row.map_err(sql)?;
            metadata.insert(key, Value::String(value));
        }
        let mut counts = Vec::new();
        let mut stmt = db.prepare("SELECT set_name AS type, COUNT(*) AS count FROM entities GROUP BY set_name ORDER BY set_name").map_err(sql)?;
        for row in stmt
            .query_map([], |r| {
                Ok(json!({"type": r.get::<_, Option<String>>(0)?, "count": r.get::<_, i64>(1)?}))
            })
            .map_err(sql)?
        {
            counts.push(row.map_err(sql)?);
        }
        Ok(json!({"metadata": metadata, "counts": counts}))
    }
}

#[derive(Default, Clone)]
pub struct LookupOptions {
    pub suit: Option<String>,
    pub edition: Option<String>,
    pub enhancement: Option<String>,
    pub seal: Option<String>,
    pub stickers: Vec<String>,
}

fn sql<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}
fn json_object(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let config: Value =
        serde_json::from_str::<Value>(&row.get::<_, String>(6)?).unwrap_or(json!({}));
    let data: Value = serde_json::from_str::<Value>(&row.get::<_, String>(7)?).unwrap_or(json!({}));
    Ok(
        json!({"key": row.get::<_, String>(0)?, "kind": row.get::<_, String>(1)?, "set_name": row.get::<_, Option<String>>(2)?, "name": row.get::<_, String>(3)?, "effect": row.get::<_, Option<String>>(4)?, "effect_template": row.get::<_, Option<String>>(5)?, "config": config, "data": data}),
    )
}
fn all_entities(db: &Connection, kind: &str) -> Result<Vec<Value>, String> {
    let mut stmt = db.prepare("SELECT key, kind, set_name, name, effect, effect_template, config_json, data_json FROM entities WHERE kind = ?1 OR set_name = ?1 COLLATE NOCASE").map_err(sql)?;
    stmt.query_map([kind], json_object)
        .map_err(sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql)
}
fn canonical(value: &str) -> String {
    let mut value = value.to_ascii_lowercase();
    for prefix in ["j_", "c_", "m_", "e_", "v_", "b_", "p_", "bl_", "tag_"] {
        if let Some(rest) = value.strip_prefix(prefix) {
            value = rest.to_string();
            break;
        }
    }
    for word in ["card", "seal", "joker", "deck", "tag", "edition"] {
        value = value.replace(word, "");
    }
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}
fn find_entity(db: &Connection, kind: &str, query: &str) -> Result<Option<Value>, String> {
    let aliases = [
        ("joker", "Joker"),
        ("tarot", "Tarot"),
        ("spectral", "Spectral"),
        ("planet", "Planet"),
        ("voucher", "Voucher"),
        ("edition", "Edition"),
        ("enhancement", "Enhanced"),
        ("deck", "Back"),
    ];
    let actual = aliases.iter().find(|(k, _)| *k == kind).map(|(_, v)| *v);
    let values = if let Some(set) = actual {
        all_entities(db, set)?
    } else {
        all_entities(db, kind)?
    };
    let exact = values
        .iter()
        .find(|v| {
            v["key"]
                .as_str()
                .is_some_and(|x| x.eq_ignore_ascii_case(query))
                || v["name"]
                    .as_str()
                    .is_some_and(|x| x.eq_ignore_ascii_case(query))
        })
        .cloned();
    Ok(exact.or_else(|| {
        values.into_iter().find(|v| {
            canonical(v["key"].as_str().unwrap_or_default()) == canonical(query)
                || canonical(v["name"].as_str().unwrap_or_default()) == canonical(query)
        })
    }))
}
fn public_entity(entity: &Value) -> Value {
    let data = entity.get("data").cloned().unwrap_or(json!({}));
    let mut attributes = Map::new();
    for key in [
        "rarity",
        "cost",
        "blueprint_compat",
        "perishable_compat",
        "eternal_compat",
        "dollars",
        "mult",
        "boss",
        "debuff",
        "unlock_condition",
        "immutable",
        "copy_behavior",
        "incompatible_with",
        "rounds",
        "dollars_per_round",
    ] {
        if let Some(value) = data.get(key) {
            attributes.insert(key.into(), value.clone());
        }
    }
    let mut out = json!({"key": entity["key"], "type": entity["set_name"].as_str().unwrap_or(entity["kind"].as_str().unwrap_or_default()), "name": entity["name"], "effect": entity["effect"], "description": entity["effect"], "config": entity["config"]});
    if !attributes.is_empty() {
        out["attributes"] = Value::Object(attributes);
    }
    out
}
fn modifier(db: &Connection, kind: &str, name: &str) -> Result<Value, String> {
    find_entity(db, kind, name)?
        .map(|v| public_entity(&v))
        .ok_or_else(|| format!("{kind} not found: {name}"))
}
fn card_rows(db: &Connection, rank: &str, suit: Option<&str>) -> Result<Vec<Value>, String> {
    let values = all_entities(db, "playing_card")?;
    Ok(values
        .into_iter()
        .filter(|v| {
            v["data"]["value"]
                .as_str()
                .is_some_and(|x| x.eq_ignore_ascii_case(rank))
                && suit.is_none_or(|s| {
                    v["data"]["suit"]
                        .as_str()
                        .is_some_and(|x| x.eq_ignore_ascii_case(s))
                })
        })
        .collect())
}
fn lookup(db: &Connection, kind: &str, name: &str, o: &LookupOptions) -> Result<Value, String> {
    let normalized = kind.to_ascii_lowercase().replace('_', "-");
    if normalized == "card" || normalized == "playing-card" {
        let (rank_name, inferred_suit) = name
            .split_once(" of ")
            .map(|(rank, suit)| (rank.trim(), Some(suit.trim().to_string())))
            .unwrap_or((name.trim(), None));
        let suit = o.suit.as_deref().or(inferred_suit.as_deref());
        let cards = card_rows(db, rank_name, suit)?;
        if cards.is_empty() {
            return Err(format!(
                "playing card not found: {}{}",
                rank_name,
                suit.map(|s| format!(" of {s}")).unwrap_or_default()
            ));
        }
        let rank = cards[0]["data"]["value"].as_str().unwrap_or(rank_name);
        let base = [
            ("Ace", 11),
            ("King", 10),
            ("Queen", 10),
            ("Jack", 10),
            ("10", 10),
            ("9", 9),
            ("8", 8),
            ("7", 7),
            ("6", 6),
            ("5", 5),
            ("4", 4),
            ("3", 3),
            ("2", 2),
        ]
        .iter()
        .find(|(r, _)| r.eq_ignore_ascii_case(rank))
        .map(|(_, n)| *n)
        .unwrap_or(0);
        let mut result = json!({"query": {"type": "playing-card", "rank": rank}, "card": {"rank": rank, "suits": cards.iter().filter_map(|c| c["data"]["suit"].as_str()).collect::<Vec<_>>(), "baseChips": base}, "modifiers": {}});
        if let Some(suit) = suit {
            result["query"]["suit"] = json!(suit);
        }
        if let Some(edition) = &o.edition {
            let e = modifier(db, "edition", edition)?;
            if e["key"] == "e_negative" {
                return Err("Negative edition does not apply to playing cards.".into());
            }
            result["modifiers"]["edition"] = e;
        }
        if let Some(enhancement) = &o.enhancement {
            let e = modifier(db, "enhancement", enhancement)?;
            let bonus = e["config"]["bonus"]
                .as_i64()
                .or_else(|| e["config"]["bonus"].as_f64().map(|n| n as i64))
                .unwrap_or(0);
            result["card"]["scoringChips"] = if e["key"] == "m_stone" {
                json!(bonus)
            } else {
                json!(base + bonus)
            };
            result["modifiers"]["enhancement"] = e;
        }
        if let Some(seal) = &o.seal {
            result["modifiers"]["seal"] = modifier(db, "seal", seal)?;
        }
        if !o.stickers.is_empty() {
            return Err("Stickers apply only to Jokers.".into());
        }
        return Ok(result);
    }
    if o.enhancement.is_some() || o.seal.is_some() {
        return Err("Enhancements and seals apply only to playing cards.".into());
    }
    let consumable = ["tarot", "planet", "spectral"].contains(&normalized.as_str());
    if o.edition.is_some() && normalized != "joker" && !consumable {
        return Err("This entity type cannot have an Edition.".into());
    }
    if !o.stickers.is_empty() && normalized != "joker" {
        return Err("Stickers apply only to Jokers.".into());
    }
    let entity =
        find_entity(db, &normalized, name)?.ok_or_else(|| format!("{kind} not found: {name}"))?;
    let mut result = json!({"query": {"type": normalized, "name": name}, "entity": public_entity(&entity), "modifiers": {}});
    if let Some(edition) = &o.edition {
        if consumable
            && !edition
                .to_ascii_lowercase()
                .trim_start_matches("e_")
                .eq("negative")
        {
            return Err("Consumables support only the Negative Edition created by Perkeo.".into());
        }
        let mut e = if consumable {
            modifier(db, "edition", "e_negative_consumable")?
        } else {
            modifier(db, "edition", edition)?
        };
        e["copyBehavior"] = json!(if e["key"] == "e_negative" {
            "Not copied by Invisible Joker or Ankh"
        } else {
            "Copied with the card"
        });
        result["modifiers"]["edition"] = e;
    }
    if !o.stickers.is_empty() {
        let lower: Vec<_> = o.stickers.iter().map(|s| s.to_ascii_lowercase()).collect();
        if lower.contains(&"eternal".into()) && lower.contains(&"perishable".into()) {
            return Err("Eternal and Perishable Stickers are mutually exclusive.".into());
        }
        let mut seen = std::collections::HashSet::new();
        if lower.iter().any(|s| !seen.insert(s)) {
            return Err("A Sticker may be applied only once.".into());
        }
        let mut stickers = Vec::new();
        for sticker in &o.stickers {
            let applied = modifier(db, "sticker", sticker)?;
            let data = entity["data"].clone();
            if applied["key"] == "eternal" && data["eternal_compat"] == false {
                return Err(format!(
                    "{} is incompatible with the Eternal Sticker.",
                    entity["name"]
                ));
            }
            if applied["key"] == "perishable" && data["perishable_compat"] == false {
                return Err(format!(
                    "{} is incompatible with the Perishable Sticker.",
                    entity["name"]
                ));
            }
            stickers.push(applied);
        }
        result["modifiers"]["stickers"] = Value::Array(stickers);
    }
    Ok(result)
}

struct LuaParser<'a> {
    source: &'a str,
    i: usize,
}
impl<'a> LuaParser<'a> {
    fn new(source: &'a str) -> Self {
        Self { source, i: 0 }
    }
    fn space(&mut self) {
        loop {
            while self.i < self.source.len() && self.source.as_bytes()[self.i].is_ascii_whitespace()
            {
                self.i += 1;
            }
            if self.source[self.i..].starts_with("--") {
                self.i = self.source[self.i..]
                    .find('\n')
                    .map(|n| self.i + n + 1)
                    .unwrap_or(self.source.len());
            } else {
                break;
            }
        }
    }
    fn value(&mut self) -> Value {
        self.space();
        let c = self
            .source
            .as_bytes()
            .get(self.i)
            .copied()
            .unwrap_or_default() as char;
        match c {
            '{' => self.table(),
            '"' | '\'' => Value::String(self.string()),
            _ => {
                let start = self.i;
                while self.i < self.source.len()
                    && (self.source.as_bytes()[self.i].is_ascii_alphanumeric()
                        || b"_.-".contains(&self.source.as_bytes()[self.i]))
                {
                    self.i += 1;
                }
                let word = &self.source[start..self.i];
                if word.contains('.') {
                    if let Ok(n) = word.parse::<f64>() {
                        return json!(n);
                    }
                } else if let Ok(n) = word.parse::<i64>() {
                    return json!(n);
                }
                if word == "true" {
                    return json!(true);
                }
                if word == "false" {
                    return json!(false);
                }
                if word == "nil" {
                    return Value::Null;
                }
                if !word.is_empty() {
                    self.space();
                    if self.source.as_bytes().get(self.i) == Some(&b'(') {
                        return self.expression(start);
                    }
                    return json!(word);
                }
                self.expression(start)
            }
        }
    }
    fn table(&mut self) -> Value {
        self.i += 1;
        let mut map = Map::new();
        let mut index = 1;
        while self.i < self.source.len() {
            self.space();
            if self.source.as_bytes().get(self.i) == Some(&b'}') {
                self.i += 1;
                break;
            }
            let start = self.i;
            let mut key = None;
            while self.i < self.source.len()
                && (self.source.as_bytes()[self.i].is_ascii_alphanumeric()
                    || self.source.as_bytes()[self.i] == b'_')
            {
                self.i += 1
            }
            if self.i > start {
                let candidate = &self.source[start..self.i];
                self.space();
                if self.source.as_bytes().get(self.i) == Some(&b'=') {
                    key = Some(candidate.to_string());
                    self.i += 1
                } else {
                    self.i = start
                }
            } else if self.source.as_bytes().get(self.i) == Some(&b'[') {
                self.i += 1;
                let k = self.value();
                self.space();
                if self.source.as_bytes().get(self.i) == Some(&b']') {
                    self.i += 1
                };
                self.space();
                if self.source.as_bytes().get(self.i) == Some(&b'=') {
                    self.i += 1
                };
                key = Some(
                    k.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| k.to_string()),
                )
            }
            let v = self.value();
            map.insert(
                key.unwrap_or_else(|| {
                    let k = index.to_string();
                    index += 1;
                    k
                }),
                v,
            );
            self.space();
            if self
                .source
                .as_bytes()
                .get(self.i)
                .is_some_and(|c| *c == b',' || *c == b';')
            {
                self.i += 1
            }
        }
        let sequential = (1..=map.len()).all(|n| map.contains_key(&n.to_string()));
        if sequential {
            Value::Array(
                (1..=map.len())
                    .map(|n| map[&n.to_string()].clone())
                    .collect(),
            )
        } else {
            Value::Object(map)
        }
    }
    fn string(&mut self) -> String {
        let quote = self.source.as_bytes()[self.i];
        self.i += 1;
        let mut out = String::new();
        while self.i < self.source.len() {
            let c = self.source.as_bytes()[self.i];
            self.i += 1;
            if c == quote {
                break;
            }
            if c == b'\\' && self.i < self.source.len() {
                let n = self.source.as_bytes()[self.i];
                self.i += 1;
                out.push(match n {
                    b'n' => '\n',
                    b'r' => '\r',
                    b't' => '\t',
                    x => x as char,
                });
            } else {
                out.push(c as char)
            }
        }
        out
    }
    fn expression(&mut self, start: usize) -> Value {
        let mut round = 0;
        let mut square = 0;
        let mut quote = 0u8;
        while self.i < self.source.len() {
            let c = self.source.as_bytes()[self.i];
            if quote != 0 {
                self.i += if c == b'\\' { 2 } else { 1 };
                if c == quote {
                    quote = 0
                };
                continue;
            }
            match c {
                b'"' | b'\'' => quote = c,
                b'(' => round += 1,
                b')' => round -= 1,
                b'[' => square += 1,
                b']' => square -= 1,
                b',' | b'}' | b';' if round <= 0 && square <= 0 => break,
                _ => {}
            }
            self.i += 1;
        }
        json!({"luaExpression":self.source[start..self.i].trim()})
    }
}
fn assigned(source: &str, marker: &str) -> Result<Value, String> {
    let at = source
        .find(marker)
        .ok_or_else(|| format!("Lua table marker not found: {marker}"))?;
    let start = source[at + marker.len()..]
        .find('{')
        .map(|n| at + marker.len() + n)
        .ok_or("Lua table not found".to_string())?;
    Ok(LuaParser::new(&source[start..]).value())
}
fn returned(source: &str) -> Result<Value, String> {
    let at = source
        .find("return")
        .ok_or("Lua return table not found".to_string())?;
    let start = source[at..]
        .find('{')
        .map(|n| at + n)
        .ok_or("Lua return table not found".to_string())?;
    Ok(LuaParser::new(&source[start..]).value())
}
fn strip_markup(s: &str) -> String {
    let mut out = String::new();
    let mut braces = 0;
    for c in s.chars() {
        if c == '{' {
            braces += 1
        } else if c == '}' && braces > 0 {
            braces -= 1
        } else if braces == 0 {
            out.push(c)
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn render(lines: &[Value], vars: &[String]) -> String {
    let text = lines
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    let text = strip_markup(&text);
    let mut out = text;
    for i in (1..=9).rev() {
        out = out.replace(
            &format!("#{i}#"),
            vars.get(i - 1)
                .map(String::as_str)
                .unwrap_or(&format!("#{i}#")),
        );
    }
    out
}
fn config_vars(key: &str, record: &Value) -> Vec<String> {
    let c = &record["config"];
    let get = |k: &str| {
        c.get(k)
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| v.as_f64().map(|n| n.to_string()))
            })
            .flatten()
    };
    let mut values = match key {
        "j_madness" => vec![
            get("extra").unwrap_or_else(|| "0".into()),
            get("Xmult").unwrap_or_else(|| "1".into()),
        ],
        "e_foil" | "e_holo" | "e_polychrome" | "e_negative" => {
            vec![get("extra").unwrap_or_else(|| "1".into())]
        }
        "m_glass" => vec![
            get("Xmult").unwrap_or_else(|| "1".into()),
            "1".into(),
            get("extra").unwrap_or_else(|| "0".into()),
        ],
        "m_steel" => vec![get("h_x_mult").unwrap_or_else(|| "1.5".into())],
        "m_gold" => vec![get("h_dollars").unwrap_or_else(|| "3".into())],
        "m_lucky" => vec![
            "1".into(),
            get("mult").unwrap_or_else(|| "20".into()),
            "5".into(),
            get("p_dollars").unwrap_or_else(|| "20".into()),
            "15".into(),
        ],
        "m_bonus" => vec![get("bonus").unwrap_or_else(|| "30".into())],
        "m_mult" => vec![get("mult").unwrap_or_else(|| "4".into())],
        _ => Vec::new(),
    };
    if values.is_empty() {
        if let Some(object) = c.as_object() {
            values.extend(object.values().filter_map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| value.as_f64().map(|n| n.to_string()))
            }));
        }
    }
    values
}
fn number_after(source: &str, marker: &str) -> i64 {
    source
        .find(marker)
        .and_then(|i| {
            source[i + marker.len()..]
                .trim_start()
                .split(|c: char| !c.is_ascii_digit())
                .next()
        })
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}
fn localized_for<'a>(loc: &'a Value, key: &str, record: &Value, kind: &str) -> Option<&'a Value> {
    let descriptions = loc.get("descriptions")?;
    if kind == "seal" {
        return descriptions
            .get("Other")
            .and_then(|v| v.get(&format!("{}_seal", key.to_ascii_lowercase())));
    }
    if kind == "blind" {
        return descriptions.get("Blind").and_then(|v| v.get(key));
    }
    let set = record["set"].as_str().unwrap_or(kind);
    if let Some(value) = descriptions.get(set).and_then(|v| v.get(key)) {
        return Some(value);
    }
    if set == "Booster" {
        let generic = key
            .strip_suffix("_1")
            .or_else(|| key.strip_suffix("_2"))
            .or_else(|| key.strip_suffix("_3"))
            .unwrap_or(key);
        return descriptions.get("Other").and_then(|v| v.get(generic));
    }
    None
}

pub fn build_database(source: &Path, database: &Path) -> Result<Value, String> {
    let game = fs::read_to_string(source.join("game.lua"))
        .map_err(|e| format!("read game.lua failed: {e}"))?;
    let loc_source = fs::read_to_string(source.join("localization").join("en-us.lua"))
        .map_err(|e| format!("read localization failed: {e}"))?;
    let loc = returned(&loc_source)?;
    let version = fs::read_to_string(source.join("version.jkr"))
        .map_err(|e| format!("read version failed: {e}"))?;
    if let Some(parent) = database.parent() {
        fs::create_dir_all(parent).map_err(sql)?;
    }
    let mut db = Connection::open(database).map_err(sql)?;
    db.execute_batch("DROP TABLE IF EXISTS entities; DROP TABLE IF EXISTS metadata; CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL); CREATE TABLE entities (key TEXT PRIMARY KEY, kind TEXT NOT NULL, set_name TEXT, name TEXT NOT NULL, effect TEXT, effect_template TEXT, config_json TEXT NOT NULL, data_json TEXT NOT NULL, source_file TEXT NOT NULL, source_marker TEXT NOT NULL); CREATE INDEX entities_kind_name ON entities(kind, name COLLATE NOCASE); CREATE INDEX entities_set_name ON entities(set_name, name COLLATE NOCASE);").map_err(sql)?;
    let tx = db.transaction().map_err(sql)?;
    let mut count = 0;
    for (marker, kind) in TABLES {
        let records = assigned(&game, marker)?
            .as_object()
            .cloned()
            .unwrap_or_default();
        for (key, record) in records {
            if !record.is_object() {
                continue;
            }
            let set = record["set"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| {
                    if *kind == "playing_card" {
                        "Playing Card".into()
                    } else {
                        (*kind).into()
                    }
                });
            let localized = localized_for(&loc, &key, &record, kind);
            let name = localized
                .and_then(|v| v.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_else(|| record["name"].as_str().unwrap_or(&key))
                .to_string();
            let config = &record["config"];
            let vars = config_vars(&key, &record);
            let lines = localized
                .and_then(|v| v.get("text"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let template = render(&lines, &[]);
            let effect = if !render(&lines, &vars).is_empty() {
                render(&lines, &vars)
            } else if key == "m_bonus" {
                format!(
                    "+{} Chips when scored",
                    record["config"]["bonus"].as_i64().unwrap_or(0)
                )
            } else {
                String::new()
            };
            let config_json = serde_json::to_string(config).unwrap_or_else(|_| "{}".into());
            let data_json = serde_json::to_string(&record).unwrap_or_else(|_| "{}".into());
            tx.execute(
                "INSERT INTO entities VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    key,
                    kind,
                    set,
                    name,
                    effect,
                    template,
                    config_json,
                    data_json,
                    "rules/balatro_src/game.lua",
                    marker
                ],
            )
            .map_err(sql)?;
            count += 1;
        }
    }
    let perishable_rounds = number_after(&game, "perishable_rounds =");
    let rental_rate = number_after(&game, "rental_rate =");
    for (key, variables, data) in [
        (
            "eternal",
            vec![],
            json!({"immutable": true, "copy_behavior": "copied", "incompatible_with": ["perishable"]}),
        ),
        (
            "perishable",
            vec![perishable_rounds.to_string(), perishable_rounds.to_string()],
            json!({"immutable": true, "copy_behavior": "copied", "rounds": perishable_rounds, "incompatible_with": ["eternal"]}),
        ),
        (
            "rental",
            vec![rental_rate.to_string()],
            json!({"immutable": true, "copy_behavior": "copied", "dollars_per_round": rental_rate}),
        ),
    ] {
        let localized = loc
            .get("descriptions")
            .and_then(|v| v.get("Other"))
            .and_then(|v| v.get(key));
        let name = localized
            .and_then(|v| v.get("name"))
            .and_then(Value::as_str)
            .unwrap_or(key);
        let lines = localized
            .and_then(|v| v.get("text"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let template = render(&lines, &[]);
        let effect = render(&lines, &variables);
        tx.execute("INSERT INTO entities VALUES (?1,'sticker','Sticker',?2,?3,?4,'{}',?5,'rules/balatro_src/localization/en-us.lua','descriptions.Other.' || ?1)", params![key, name, effect, template, serde_json::to_string(&data).unwrap_or_else(|_| "{}".into())]).map_err(sql)?;
        count += 1;
    }
    let localized = loc
        .get("descriptions")
        .and_then(|v| v.get("Edition"))
        .and_then(|v| v.get("e_negative_consumable"));
    let name = localized
        .and_then(|v| v.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("Negative");
    let lines = localized
        .and_then(|v| v.get("text"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    tx.execute("INSERT INTO entities VALUES ('e_negative_consumable','edition','Edition',?1,?2,?3,?4,?5,'rules/balatro_src/localization/en-us.lua','descriptions.Edition.e_negative_consumable')", params![name, render(&lines, &["1".into()]), render(&lines, &[]), r#"{"extra":1}"#, r#"{"applies_to":["Tarot","Planet","Spectral"],"source_joker":"j_perkeo"}"#]).map_err(sql)?;
    count += 1;
    tx.execute(
        "INSERT INTO metadata VALUES ('balatro_version', ?1)",
        params![version.trim()],
    )
    .map_err(sql)?;
    tx.execute(
        "INSERT INTO metadata VALUES ('entity_count', ?1)",
        params![count.to_string()],
    )
    .map_err(sql)?;
    tx.execute("INSERT INTO metadata VALUES ('source', 'rules/balatro_src Lua prototypes and en-us localization')", []).map_err(sql)?;
    tx.commit().map_err(sql)?;
    Ok(json!({"database": database, "entities": count, "balatroVersion": version.trim()}))
}

pub fn cli(root: &Path, args: &[String]) -> Result<Option<Value>, String> {
    if args.first().map(String::as_str) != Some("rules") {
        return Ok(None);
    }
    let command = args
        .get(1)
        .map(String::as_str)
        .ok_or("usage: rules build|lookup|list|stats".to_string())?;
    let db = args
        .iter()
        .position(|a| a == "--db")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| default_db_path(root));
    match command {
        "build" => {
            let source = args
                .iter()
                .position(|a| a == "--source")
                .and_then(|i| args.get(i + 1))
                .map(PathBuf::from)
                .unwrap_or_else(|| default_source_path(root));
            Ok(Some(build_database(&source, &db)?))
        }
        "stats" => Ok(Some(RulesDb::new(db).stats()?)),
        "list" => Ok(Some(
            RulesDb::new(db).list(
                args.get(2)
                    .filter(|a| !a.starts_with("--"))
                    .map(String::as_str),
            )?,
        )),
        "lookup" => {
            let kind = args
                .get(2)
                .ok_or("usage: rules lookup <type> <name> [modifiers]".to_string())?;
            let name = args
                .get(3)
                .ok_or("usage: rules lookup <type> <name> [modifiers]".to_string())?;
            let mut options = LookupOptions::default();
            let mut i = 4;
            while i < args.len() {
                let flag = args.get(i).ok_or("missing option".to_string())?;
                let value = args
                    .get(i + 1)
                    .ok_or_else(|| format!("missing value for {flag}"))?
                    .clone();
                if flag == "--db" {
                    i += 2;
                    continue;
                }
                match flag.as_str() {
                    "--suit" => options.suit = Some(value),
                    "--edition" => options.edition = Some(value),
                    "--enhancement" => options.enhancement = Some(value),
                    "--seal" => options.seal = Some(value),
                    "--sticker" => options.stickers.push(value),
                    _ => return Err(format!("unknown lookup option: {flag}")),
                }
                i += 2;
            }
            Ok(Some(RulesDb::new(db).lookup(kind, name, &options)?))
        }
        _ => Err(format!("unknown rules command: {command}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lua_parser_handles_tables_strings_comments_and_expressions() {
        let value = LuaParser::new(r#"{ name = "A\nB", values = {1, true, nil}, nested = { answer = foo(bar, {x = 1}) }, -- comment
            ["key"] = 'value' }"#).value();
        assert_eq!(value["name"], "A\nB");
        assert_eq!(value["values"][0], 1);
        assert_eq!(value["values"][1], true);
        assert_eq!(
            value["nested"]["answer"]["luaExpression"],
            "foo(bar, {x = 1})"
        );
        assert_eq!(value["key"], "value");
    }

    #[test]
    fn lookup_preserves_modifier_contracts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.sqlite");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE entities (key TEXT PRIMARY KEY, kind TEXT NOT NULL, set_name TEXT, name TEXT NOT NULL, effect TEXT, effect_template TEXT, config_json TEXT NOT NULL, data_json TEXT NOT NULL, source_file TEXT NOT NULL, source_marker TEXT NOT NULL); CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);").unwrap();
        db.execute("INSERT INTO entities VALUES ('j_test','center','Joker','Test Joker','+4 Mult','+#1# Mult','{\"mult\":4}','{\"name\":\"Test Joker\",\"eternal_compat\":true,\"perishable_compat\":false}','test','test')", []).unwrap();
        db.execute("INSERT INTO entities VALUES ('m_bonus','center','Enhanced','Bonus','+30 Chips','+30 Chips','{\"bonus\":30}','{\"name\":\"Bonus\"}','test','test')", []).unwrap();
        db.execute("INSERT INTO entities VALUES ('eternal','sticker','Sticker','Eternal','Cannot be sold','Cannot be sold','{}','{\"immutable\":true}','test','test')", []).unwrap();
        db.execute("INSERT INTO entities VALUES ('perishable','sticker','Sticker','Perishable','Debuffed','Debuffed','{}','{\"immutable\":true}','test','test')", []).unwrap();
        drop(db);
        let rules = RulesDb::new(path);
        let value = rules
            .lookup(
                "joker",
                "test joker",
                &LookupOptions {
                    stickers: vec!["Eternal".into()],
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(value["entity"]["key"], "j_test");
        assert_eq!(value["modifiers"]["stickers"][0]["key"], "eternal");
        let error = rules
            .lookup(
                "joker",
                "j_test",
                &LookupOptions {
                    stickers: vec!["Perishable".into()],
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(error.contains("incompatible"));
    }
}
