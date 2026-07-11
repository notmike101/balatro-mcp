/** A small parser for the literal-table subset used by Balatro prototype data. */
export class LuaParser {
  /** @param {string} source Lua expression source. */
  constructor(source) { this.source = source; this.i = 0; }

  /** Parse one Lua value. Unsupported expressions are retained as source strings. */
  parseValue() {
    this.#space();
    const c = this.source[this.i];
    if (c === '{') return this.#table();
    if (c === '"' || c === "'") return this.#string();
    const number = this.source.slice(this.i).match(/^-?\d+(?:\.\d+)?/);
    if (number) { this.i += number[0].length; return Number(number[0]); }
    const word = this.source.slice(this.i).match(/^[A-Za-z_][\w.]*/)?.[0];
    if (word) {
      this.i += word.length;
      if (word === 'true') return true;
      if (word === 'false') return false;
      if (word === 'nil') return null;
      this.#space();
      if (this.source[this.i] === '(') return this.#expression(word);
      return word;
    }
    return this.#expression('');
  }

  #table() {
    this.i++;
    const object = {};
    let arrayIndex = 1;
    while (this.i < this.source.length) {
      this.#space();
      if (this.source[this.i] === '}') { this.i++; break; }
      const start = this.i;
      let key;
      const identifier = this.source.slice(this.i).match(/^[A-Za-z_][\w]*/)?.[0];
      if (identifier) {
        this.i += identifier.length; this.#space();
        if (this.source[this.i] === '=') { key = identifier; this.i++; }
        else this.i = start;
      } else if (this.source[this.i] === '[') {
        this.i++; key = String(this.parseValue()); this.#space();
        if (this.source[this.i] === ']') this.i++;
        this.#space(); if (this.source[this.i] === '=') this.i++;
      }
      const value = this.parseValue();
      object[key ?? String(arrayIndex++)] = value;
      this.#space();
      if (this.source[this.i] === ',' || this.source[this.i] === ';') this.i++;
    }
    const keys = Object.keys(object);
    return keys.length > 0 && keys.every((key, index) => key === String(index + 1))
      ? keys.map((key) => object[key]) : object;
  }

  #string() {
    const quote = this.source[this.i++];
    let result = '';
    while (this.i < this.source.length) {
      const c = this.source[this.i++];
      if (c === quote) break;
      if (c === '\\') {
        const next = this.source[this.i++];
        result += ({ n: '\n', r: '\r', t: '\t' })[next] ?? next;
      } else result += c;
    }
    return result;
  }

  #expression(prefix) {
    const start = this.i - prefix.length;
    let round = 0; let square = 0; let quote = null;
    while (this.i < this.source.length) {
      const c = this.source[this.i];
      if (quote) {
        if (c === '\\') this.i += 2;
        else { this.i++; if (c === quote) quote = null; }
        continue;
      }
      if (c === '"' || c === "'") { quote = c; this.i++; continue; }
      if (c === '(') round++;
      else if (c === ')') round--;
      else if (c === '[') square++;
      else if (c === ']') square--;
      else if ((c === ',' || c === '}' || c === ';') && round <= 0 && square <= 0) break;
      this.i++;
    }
    return { luaExpression: this.source.slice(start, this.i).trim() };
  }

  #space() {
    while (this.i < this.source.length) {
      if (/\s/.test(this.source[this.i])) { this.i++; continue; }
      if (this.source.startsWith('--', this.i)) {
        const end = this.source.indexOf('\n', this.i);
        this.i = end < 0 ? this.source.length : end + 1;
        continue;
      }
      break;
    }
  }
}

/** Extract and parse a balanced Lua table assigned after a marker. */
export function parseAssignedTable(source, marker) {
  const markerIndex = source.indexOf(marker);
  if (markerIndex < 0) throw new Error(`Lua table marker not found: ${marker}`);
  const start = source.indexOf('{', markerIndex + marker.length);
  return new LuaParser(source.slice(start)).parseValue();
}

/** Parse the table returned by a localization file. */
export function parseReturnedTable(source) {
  const start = source.indexOf('{', source.indexOf('return'));
  if (start < 0) throw new Error('Lua return table not found');
  return new LuaParser(source.slice(start)).parseValue();
}
