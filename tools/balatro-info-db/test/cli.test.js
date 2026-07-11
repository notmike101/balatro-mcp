import test, { after, before } from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from 'node:sqlite';
import { buildDatabase } from '../src/importer.js';
import { lookup } from '../src/query.js';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
let directory;
let DB;

before(async () => {
  directory = await mkdtemp(path.join(tmpdir(), 'balatro-info-'));
  DB = path.join(directory, 'test.sqlite');
  await buildDatabase({ sourceDir: path.join(ROOT, 'balatro_src'), databasePath: DB });
});

after(async () => rm(directory, { recursive: true, force: true }));

test('builds source database and resolves Polychrome Madness', async () => {
  const db = new DatabaseSync(DB, { readOnly: true });
  const value = lookup(db, 'joker', 'Madness', { edition: 'Polychrome' });
  db.close();
  assert.equal(value.entity.effect, 'When Small Blind or Big Blind is selected, gain X0.5 Mult and destroy a random Joker (Currently X1 Mult)');
  assert.equal(value.modifiers.edition.effect, 'X1.5 Mult');
});

test('resolves an enhanced and sealed Ace', () => {
  const db = new DatabaseSync(DB, { readOnly: true });
  const value = lookup(db, 'card', 'Ace', { enhancement: 'Bonus', seal: 'Red' });
  db.close();
  assert.equal(value.card.baseChips, 11);
  assert.equal(value.card.scoringChips, 41);
  assert.equal(value.modifiers.enhancement.config.bonus, 30);
  assert.match(value.modifiers.seal.effect, /Retrigger/);
});

test('applies compatible, repeatable Joker Stickers', () => {
  const db = new DatabaseSync(DB, { readOnly: true });
  const value = lookup(db, 'joker', 'Madness', { sticker: ['Eternal', 'Rental'] });
  assert.deepEqual(value.modifiers.stickers.map((sticker) => sticker.key), ['eternal', 'rental']);
  assert.throws(() => lookup(db, 'joker', 'Madness', { sticker: ['Perishable'] }), /incompatible/);
  assert.throws(() => lookup(db, 'joker', 'Joker', { sticker: ['Eternal', 'Perishable'] }), /mutually exclusive/);
  db.close();
});

test('allows only Negative Edition on consumables and no modifiers on Vouchers', () => {
  const db = new DatabaseSync(DB, { readOnly: true });
  const value = lookup(db, 'tarot', 'The Fool', { edition: 'Negative' });
  assert.equal(value.modifiers.edition.key, 'e_negative_consumable');
  assert.equal(value.modifiers.edition.effect, '+1 consumable slot');
  assert.throws(() => lookup(db, 'planet', 'Pluto', { edition: 'Foil' }), /only the Negative/);
  assert.throws(() => lookup(db, 'voucher', 'Blank', { edition: 'Negative' }), /cannot have an Edition/);
  db.close();
});
