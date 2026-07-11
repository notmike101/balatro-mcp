import { readFile, mkdir } from 'node:fs/promises';
import path from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { parseAssignedTable, parseReturnedTable } from './lua-parser.js';
import { localizationVariables, renderText } from './text.js';

const TABLES = [
  ['self.P_CENTERS =', 'center'], ['self.P_BLINDS =', 'blind'],
  ['self.P_TAGS =', 'tag'], ['self.P_SEALS =', 'seal'],
  ['self.P_STAKES =', 'stake'], ['self.P_CARDS =', 'playing_card'],
];

/**
 * Infer localization for a prototype from its set and key.
 * @param {object} localization Parsed English localization table.
 * @param {string} key Prototype key.
 * @param {object} record Prototype record.
 * @param {string} kind Import table kind.
 * @returns {object|undefined} Matching localization record.
 */
function localizationFor(localization, key, record, kind) {
  if (kind === 'seal') return localization.descriptions?.Other?.[`${key.toLowerCase()}_seal`];
  if (kind === 'blind') return localization.descriptions?.Blind?.[key];
  const set = record.set;
  const exact = localization.descriptions?.[set]?.[key];
  if (exact) return exact;
  if (set === 'Booster') {
    const genericKey = key.replace(/_\d+$/, '').replace(/_(normal|jumbo|mega)$/, '_$1');
    return localization.descriptions?.Other?.[genericKey];
  }
  return undefined;
}

/**
 * Build a fresh SQLite database exclusively from the checked-in Lua source.
 * @param {{sourceDir: string, databasePath: string}} paths Input and output paths.
 * @returns {Promise<{database: string, entities: number, balatroVersion: string}>} Build summary.
 */
export async function buildDatabase({ sourceDir, databasePath }) {
  const gamePath = path.join(sourceDir, 'game.lua');
  const localizationPath = path.join(sourceDir, 'localization', 'en-us.lua');
  const [gameSource, localizationSource, version] = await Promise.all([
    readFile(gamePath, 'utf8'), readFile(localizationPath, 'utf8'),
    readFile(path.join(sourceDir, 'version.jkr'), 'utf8'),
  ]);
  const localization = parseReturnedTable(localizationSource);
  await mkdir(path.dirname(databasePath), { recursive: true });
  const db = new DatabaseSync(databasePath);
  db.exec(`
    PRAGMA journal_mode = WAL;
    DROP TABLE IF EXISTS entities;
    DROP TABLE IF EXISTS metadata;
    CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE entities (
      key TEXT PRIMARY KEY, kind TEXT NOT NULL, set_name TEXT,
      name TEXT NOT NULL, effect TEXT, effect_template TEXT,
      config_json TEXT NOT NULL, data_json TEXT NOT NULL,
      source_file TEXT NOT NULL, source_marker TEXT NOT NULL
    );
    CREATE INDEX entities_kind_name ON entities(kind, name COLLATE NOCASE);
    CREATE INDEX entities_set_name ON entities(set_name, name COLLATE NOCASE);
  `);
  const insert = db.prepare(`INSERT INTO entities
    (key, kind, set_name, name, effect, effect_template, config_json, data_json, source_file, source_marker)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`);
  let count = 0;
  db.exec('BEGIN');
  try {
    for (const [marker, kind] of TABLES) {
      const records = parseAssignedTable(gameSource, marker);
      for (const [key, record] of Object.entries(records)) {
        if (!record || typeof record !== 'object' || Array.isArray(record)) continue;
        const loc = localizationFor(localization, key, record, kind);
        const lines = Array.isArray(loc?.text) ? loc.text : [];
        const template = renderText(lines);
        const effect = renderText(lines, localizationVariables(key, record)) ||
          (key === 'm_bonus' ? `+${record.config?.bonus} Chips when scored` : '');
        insert.run(key, kind, record.set ?? (kind === 'playing_card' ? 'Playing Card' : kind),
          loc?.name ?? record.name ?? key, effect || null, template || null,
          JSON.stringify(record.config ?? {}), JSON.stringify(record),
          'balatro_src/game.lua', marker.trim());
        count++;
      }
    }
    const perishableRounds = Number(gameSource.match(/perishable_rounds\s*=\s*(\d+)/)?.[1]);
    const rentalRate = Number(gameSource.match(/rental_rate\s*=\s*(\d+)/)?.[1]);
    const stickerDefinitions = [
      ['eternal', [], { immutable: true, copy_behavior: 'copied', incompatible_with: ['perishable'] }],
      ['perishable', [perishableRounds, perishableRounds], { immutable: true, copy_behavior: 'copied', rounds: perishableRounds, incompatible_with: ['eternal'] }],
      ['rental', [rentalRate], { immutable: true, copy_behavior: 'copied', dollars_per_round: rentalRate }],
    ];
    for (const [key, variables, data] of stickerDefinitions) {
      const loc = localization.descriptions?.Other?.[key];
      insert.run(key, 'sticker', 'Sticker', loc?.name ?? key, renderText(loc?.text, variables),
        renderText(loc?.text), '{}', JSON.stringify(data), 'balatro_src/localization/en-us.lua', `descriptions.Other.${key}`);
      count++;
    }
    const negativeConsumable = localization.descriptions?.Edition?.e_negative_consumable;
    insert.run('e_negative_consumable', 'edition', 'Edition', negativeConsumable.name,
      renderText(negativeConsumable.text, [1]), renderText(negativeConsumable.text),
      JSON.stringify({ extra: 1 }), JSON.stringify({ applies_to: ['Tarot', 'Planet', 'Spectral'], source_joker: 'j_perkeo' }),
      'balatro_src/localization/en-us.lua', 'descriptions.Edition.e_negative_consumable');
    count++;
    db.prepare('INSERT INTO metadata VALUES (?, ?)').run('balatro_version', version.trim());
    db.prepare('INSERT INTO metadata VALUES (?, ?)').run('entity_count', String(count));
    db.prepare('INSERT INTO metadata VALUES (?, ?)').run('source', 'balatro_src Lua prototypes and en-us localization');
    db.exec('COMMIT');
  } catch (error) { db.exec('ROLLBACK'); db.close(); throw error; }
  db.exec('PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode = DELETE;');
  db.close();
  return { database: databasePath, entities: count, balatroVersion: version.trim() };
}
