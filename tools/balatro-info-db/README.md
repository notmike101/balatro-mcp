# Balatro Info DB

A reproducible SQLite knowledge base and Node.js CLI derived exclusively from the Lua files in `balatro_src`. It imports Jokers, consumables, editions, enhancements, decks, boosters, blinds, tags, seals, stakes, and all 52 playing cards, together with English effect text and raw prototype configuration.

## Local setup

Node.js 22.5 or newer is required for the built-in `node:sqlite` module. There are no npm dependencies. This public repository intentionally does not distribute Balatro source files or a generated database. Copy the Lua source files from your own legitimate installation to `tools/balatro-info-db/balatro_src`, then build locally:

```powershell
npm run build-db
node .\bin\balatro-info.js --help
```

The generated database is `data/balatro.sqlite`. Re-run `npm run build-db` whenever your local source changes. Gameplay agents must access this database only through the Rust MCP `lookup_rule`, `list_rules`, and `rules_stats` tools.

## Queries

```powershell
node .\bin\balatro-info.js lookup joker Madness --edition Polychrome
node .\bin\balatro-info.js lookup joker Madness --sticker Eternal --sticker Rental
node .\bin\balatro-info.js lookup card Ace --enhancement Bonus --seal Red
node .\bin\balatro-info.js lookup tarot "The Fool"
node .\bin\balatro-info.js lookup tarot "The Fool" --edition Negative
node .\bin\balatro-info.js lookup blind "The Ox"
node .\bin\balatro-info.js list Joker
node .\bin\balatro-info.js stats
```

Lookup accepts a source key (such as `j_madness`) or an exact case-insensitive display name. Jokers may have one Edition and multiple compatible Stickers; `--sticker` is repeatable, while Eternal and Perishable are mutually exclusive. Tarot, Planet, and Spectral consumables support only the Negative Edition produced by Perkeo. Playing cards may have one non-Negative Edition, one Enhancement, and one Seal. Vouchers accept no modifiers. Invalid combinations fail with a JSON error and nonzero exit status. Output is structured JSON.

## Database schema

`entities` stores a normalized key, kind/set/name, rendered effect, unrendered localization template, raw config JSON, complete Lua prototype JSON, and source provenance. `metadata` records the source version and record count. The raw JSON preserves fields that are useful but not common enough to warrant dedicated columns. Lookup output surfaces common attributes such as rarity, cost, compatibility, blind reward/multiplier, boss constraints, and unlock conditions.

The importer uses a purpose-built parser for Balatro's literal Lua prototype tables. It does not execute game code. Function-call expressions that occur inside data are preserved as structured `luaExpression` values rather than evaluated.
