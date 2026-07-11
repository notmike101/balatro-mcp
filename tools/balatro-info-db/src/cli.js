import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { buildDatabase } from './importer.js';
import { openDatabase } from './database.js';
import { lookup } from './query.js';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const DEFAULT_DB = path.join(ROOT, 'data', 'balatro.sqlite');
const HELP = `Balatro Info DB

Usage:
  balatro-info build [--source PATH] [--db PATH]
  balatro-info lookup <type> <name> [modifiers] [--db PATH]
  balatro-info list [type] [--db PATH]
  balatro-info stats [--db PATH]

Types:
  joker, tarot, spectral, planet, voucher, deck, blind, tag, seal,
  edition, enhancement, sticker, stake, card (or playing-card)

Applicable modifiers:
  --edition NAME       Joker or playing card; Consumables support Negative only
  --enhancement NAME   Playing card only (Bonus, Mult, Wild Card, Glass Card, etc.)
  --seal NAME          Playing card only (Gold, Red, Blue, Purple)
  --sticker NAME       Joker only; repeat for multiple (Eternal, Perishable, Rental)
  --suit NAME          Restrict a playing-card rank to one suit

Examples:
  balatro-info lookup joker Madness --edition Polychrome
  balatro-info lookup joker Madness --sticker Eternal --sticker Rental
  balatro-info lookup card Ace --enhancement Bonus --seal Red
  balatro-info lookup tarot "The Fool"
  balatro-info lookup tarot "The Fool" --edition Negative
  balatro-info lookup blind "The Ox"

All successful commands emit JSON to stdout. Data is rebuilt solely from ./balatro_src.`;

/** @param {string[]} argv Raw CLI arguments. @returns {{positional: string[], options: object}} Parsed arguments. */
function parseArgs(argv) {
  const positional = []; const options = {};
  for (let i = 0; i < argv.length; i++) {
    if (!argv[i].startsWith('--')) positional.push(argv[i]);
    else {
      const key = argv[i].slice(2);
      if (key === 'help') options.help = true;
      else {
        if (!argv[i + 1] || argv[i + 1].startsWith('--')) throw new Error(`Missing value for --${key}`);
        const value = argv[++i];
        if (key === 'sticker') options.sticker = [...(options.sticker ?? []), value];
        else {
          if (options[key] !== undefined) throw new Error(`--${key} may be provided only once.`);
          options[key] = value;
        }
      }
    }
  }
  return { positional, options };
}

/** @param {string[]} argv CLI arguments excluding the Node executable and script. */
export async function main(argv) {
  const { positional, options } = parseArgs(argv);
  if (options.help || positional.length === 0 || positional[0] === 'help') { process.stdout.write(`${HELP}\n`); return; }
  const command = positional[0];
  const databasePath = path.resolve(options.db ?? DEFAULT_DB);
  if (command === 'build') {
    const result = await buildDatabase({ sourceDir: path.resolve(options.source ?? path.join(ROOT, 'balatro_src')), databasePath });
    process.stdout.write(`${JSON.stringify(result, null, 2)}\n`); return;
  }
  const db = openDatabase(databasePath);
  try {
    if (command === 'lookup') {
      if (!positional[1] || !positional[2]) throw new Error('Usage: lookup <type> <name> [modifiers]');
      process.stdout.write(`${JSON.stringify(lookup(db, positional[1], positional.slice(2).join(' '), options), null, 2)}\n`);
    } else if (command === 'list') {
      const type = positional[1];
      const rows = type
        ? db.prepare('SELECT key, set_name AS type, name FROM entities WHERE kind = ? OR set_name = ? COLLATE NOCASE ORDER BY name').all(type, type)
        : db.prepare('SELECT key, set_name AS type, name FROM entities ORDER BY set_name, name').all();
      process.stdout.write(`${JSON.stringify({ count: rows.length, entities: rows }, null, 2)}\n`);
    } else if (command === 'stats') {
      const metadata = Object.fromEntries(db.prepare('SELECT key, value FROM metadata').all().map((row) => [row.key, row.value]));
      const counts = db.prepare('SELECT set_name AS type, COUNT(*) AS count FROM entities GROUP BY set_name ORDER BY set_name').all();
      process.stdout.write(`${JSON.stringify({ metadata, counts }, null, 2)}\n`);
    } else throw new Error(`Unknown command: ${command}. Use --help.`);
  } finally { db.close(); }
}
