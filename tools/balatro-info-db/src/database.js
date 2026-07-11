import { existsSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';

/** @param {string} databasePath Database path. @returns {DatabaseSync} Read-only database. */
export function openDatabase(databasePath) {
  if (!existsSync(databasePath)) throw new Error('Database missing. Run `npm run build-db` first.');
  return new DatabaseSync(databasePath, { readOnly: true });
}

/** @param {object|undefined} row SQLite row. @returns {object|null} Parsed entity. */
export function hydrate(row) {
  if (!row) return null;
  return { ...row, config: JSON.parse(row.config_json), data: JSON.parse(row.data_json) };
}

/**
 * Resolve an entity by key, exact name, or conventional short name.
 * @param {DatabaseSync} db Open database.
 * @param {string} kind Entity kind.
 * @param {string} query Key or display name.
 * @returns {object|null} Matching entity.
 */
export function findEntity(db, kind, query) {
  const aliases = { joker: 'Joker', tarot: 'Tarot', spectral: 'Spectral', planet: 'Planet',
    voucher: 'Voucher', edition: 'Edition', enhancement: 'Enhanced', deck: 'Back' };
  const normalized = aliases[kind.toLowerCase()];
  const candidates = normalized
    ? db.prepare('SELECT * FROM entities WHERE set_name = ? COLLATE NOCASE OR kind = ? COLLATE NOCASE').all(normalized, kind)
    : db.prepare('SELECT * FROM entities WHERE kind = ? COLLATE NOCASE').all(kind);
  const canonical = (value) => value.toLowerCase()
    .replace(/^(j|c|m|e|v|b|p|bl|tag)_/, '')
    .replace(/\b(card|seal|joker|deck|tag|edition)\b/g, '')
    .replace(/[^a-z0-9]/g, '');
  const wanted = canonical(query);
  return hydrate(candidates.find((row) => row.key.toLowerCase() === query.toLowerCase() ||
    row.name.toLowerCase() === query.toLowerCase()) ??
    candidates.find((row) => canonical(row.key) === wanted || canonical(row.name) === wanted));
}

/**
 * Find playing cards by rank and optional suit.
 * @param {DatabaseSync} db Open database.
 * @param {string} rank Rank display name.
 * @param {string|undefined} suit Optional suit.
 * @returns {object[]} Matching playing cards.
 */
export function findPlayingCard(db, rank, suit) {
  const rows = db.prepare(`SELECT * FROM entities WHERE kind = 'playing_card'`).all().map(hydrate)
    .filter((row) => row.data.value.toLowerCase() === rank.toLowerCase() && (!suit || row.data.suit.toLowerCase() === suit.toLowerCase()));
  return rows;
}
