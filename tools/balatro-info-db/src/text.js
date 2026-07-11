/** Remove Balatro localization styling tokens while preserving visible text. */
export function stripMarkup(text) {
  return text.replace(/\{[^}]*}/g, '').replace(/\s+/g, ' ').trim();
}

/** Render localization text with known positional variables. */
export function renderText(lines = [], variables = []) {
  return stripMarkup(lines.join(' ')).replace(/#(\d+)#/g, (match, index) =>
    variables[Number(index) - 1] ?? match);
}

/** Convert a Balatro center config into common localization variables. */
export function localizationVariables(key, record) {
  const config = record.config ?? {};
  if (key === 'j_madness') return [config.extra, config.Xmult ?? 1];
  if (key === 'e_foil' || key === 'e_holo' || key === 'e_polychrome' || key === 'e_negative') return [config.extra];
  if (key === 'm_glass') return [config.Xmult, 1, config.extra];
  if (key === 'm_steel') return [config.h_x_mult];
  if (key === 'm_gold') return [config.h_dollars];
  if (key === 'm_lucky') return [1, config.mult, 5, config.p_dollars, 15];
  if (key === 'm_bonus') return [config.bonus];
  if (key === 'm_mult') return [config.mult];
  const values = Object.values(config).filter((value) => ['string', 'number'].includes(typeof value));
  return values;
}
