import { findEntity, findPlayingCard } from './database.js';

const RANK_CHIPS = { Ace: 11, King: 10, Queen: 10, Jack: 10, '10': 10, '9': 9, '8': 8, '7': 7, '6': 6, '5': 5, '4': 4, '3': 3, '2': 2 };

/** @param {object} entity Internal entity. @returns {object} Stable public fields. */
function publicEntity(entity) {
  const attributeKeys = ['rarity', 'cost', 'blueprint_compat', 'perishable_compat',
    'eternal_compat', 'dollars', 'mult', 'boss', 'debuff', 'unlock_condition',
    'immutable', 'copy_behavior', 'incompatible_with', 'rounds', 'dollars_per_round'];
  const attributes = Object.fromEntries(attributeKeys
    .filter((key) => entity.data[key] !== undefined)
    .map((key) => [key, entity.data[key]]));
  return { key: entity.key, type: entity.set_name ?? entity.kind, name: entity.name,
    effect: entity.effect, ...(Object.keys(attributes).length && { attributes }), config: entity.config };
}

/** @param {object} db Database. @param {string} type Modifier type. @param {string} name Name. */
function modifier(db, type, name) {
  const entity = findEntity(db, type, name);
  if (!entity) throw new Error(`${type} not found: ${name}`);
  return publicEntity(entity);
}

/**
 * Execute a validated lookup and return JSON-ready data.
 * @param {object} db Open database.
 * @param {string} type Entity type.
 * @param {string} name Entity name or key.
 * @param {{suit?: string, edition?: string, enhancement?: string, seal?: string, sticker?: string[]}} options Modifiers.
 * @returns {object} Structured query result.
 */
export function lookup(db, type, name, options = {}) {
  const normalizedType = type.toLowerCase().replace('_', '-');
  const stickers = options.sticker === undefined ? [] :
    (Array.isArray(options.sticker) ? options.sticker : [options.sticker]);
  if (normalizedType === 'card' || normalizedType === 'playing-card') {
    const cards = findPlayingCard(db, name, options.suit);
    if (!cards.length) throw new Error(`playing card not found: ${name}${options.suit ? ` of ${options.suit}` : ''}`);
    const result = {
      query: { type: 'playing-card', rank: name, ...(options.suit && { suit: options.suit }) },
      card: { rank: cards[0].data.value, suits: cards.map((card) => card.data.suit), baseChips: RANK_CHIPS[cards[0].data.value] },
      modifiers: {},
    };
    if (options.edition) {
      result.modifiers.edition = modifier(db, 'edition', options.edition);
      if (result.modifiers.edition.key === 'e_negative') throw new Error('Negative edition does not apply to playing cards.');
    }
    if (options.enhancement) {
      result.modifiers.enhancement = modifier(db, 'enhancement', options.enhancement);
      const config = result.modifiers.enhancement.config;
      result.card.scoringChips = result.modifiers.enhancement.key === 'm_stone'
        ? config.bonus : result.card.baseChips + (config.bonus ?? 0);
    }
    if (options.seal) result.modifiers.seal = modifier(db, 'seal', options.seal);
    if (stickers.length) throw new Error('Stickers apply only to Jokers.');
    return result;
  }
  if (options.enhancement || options.seal) throw new Error('Enhancements and seals apply only to playing cards.');
  const consumable = ['tarot', 'planet', 'spectral'].includes(normalizedType);
  if (options.edition && normalizedType !== 'joker' && !consumable) throw new Error('This entity type cannot have an Edition.');
  if (stickers.length && normalizedType !== 'joker') throw new Error('Stickers apply only to Jokers.');
  const entity = findEntity(db, normalizedType, name);
  if (!entity) throw new Error(`${type} not found: ${name}`);
  const result = { query: { type: normalizedType, name }, entity: publicEntity(entity), modifiers: {} };
  if (options.edition) {
    if (consumable && options.edition.toLowerCase().replace(/^e_/, '') !== 'negative')
      throw new Error('Consumables support only the Negative Edition created by Perkeo.');
    result.modifiers.edition = consumable
      ? modifier(db, 'edition', 'e_negative_consumable')
      : modifier(db, 'edition', options.edition);
    result.modifiers.edition.copyBehavior = result.modifiers.edition.key === 'e_negative'
      ? 'Not copied by Invisible Joker or Ankh' : 'Copied with the card';
  }
  if (stickers.length) {
    const canonical = stickers.map((value) => value.toLowerCase());
    if (canonical.includes('eternal') && canonical.includes('perishable'))
      throw new Error('Eternal and Perishable Stickers are mutually exclusive.');
    if (new Set(canonical).size !== canonical.length) throw new Error('A Sticker may be applied only once.');
    result.modifiers.stickers = stickers.map((sticker) => {
      const applied = modifier(db, 'sticker', sticker);
      if (applied.key === 'eternal' && entity.data.eternal_compat === false)
        throw new Error(`${entity.name} is incompatible with the Eternal Sticker.`);
      if (applied.key === 'perishable' && entity.data.perishable_compat === false)
        throw new Error(`${entity.name} is incompatible with the Perishable Sticker.`);
      return applied;
    });
  }
  return result;
}
