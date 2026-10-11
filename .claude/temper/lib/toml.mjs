// Minimal TOML 1.0 reader for Cargo manifests. Zero dependencies.
// Returns plain objects; arrays of tables are arrays. Dates and times are kept
// as their source text. parseToml returns null on a syntax error, so callers
// can fall back to line-based checks instead of guessing.

const BARE = /[A-Za-z0-9_-]/;
const ESC = { b: '\b', t: '\t', n: '\n', f: '\f', r: '\r', '"': '"', '\\': '\\' };

class TomlError extends Error {}

class Reader {
  constructor(src) {
    this.s = src.replace(/^﻿/, '');
    this.i = 0;
  }

  fail(what) {
    const line = this.s.slice(0, this.i).split('\n').length;
    throw new TomlError(`${what} at line ${line}`);
  }

  peek(str) { return this.s.startsWith(str, this.i); }

  expect(str) {
    if (!this.peek(str)) this.fail(`expected ${str}`);
    this.i += str.length;
  }

  ws() { while (this.s[this.i] === ' ' || this.s[this.i] === '\t') this.i++; }

  comment() {
    if (this.s[this.i] !== '#') return;
    while (this.i < this.s.length && this.s[this.i] !== '\n') this.i++;
  }

  blank() {
    for (;;) {
      this.ws();
      this.comment();
      const c = this.s[this.i];
      if (c === '\n' || c === '\r') { this.i++; continue; }
      return;
    }
  }

  lineEnd() {
    this.ws();
    this.comment();
    if (this.i >= this.s.length) return;
    if (this.peek('\r\n')) { this.i += 2; return; }
    if (this.s[this.i] === '\n') { this.i++; return; }
    this.fail('expected end of line');
  }

  document() {
    const root = {};
    let cur = root;
    for (;;) {
      this.blank();
      if (this.i >= this.s.length) return root;
      if (this.s[this.i] === '[') {
        const isArray = this.peek('[[');
        this.i += isArray ? 2 : 1;
        const keys = this.key();
        this.expect(isArray ? ']]' : ']');
        cur = isArray ? arrayTable(root, keys) : table(root, keys);
      } else {
        const keys = this.key();
        this.expect('=');
        this.ws();
        assign(cur, keys, this.value());
      }
      this.lineEnd();
    }
  }

  key() {
    const out = [];
    for (;;) {
      this.ws();
      out.push(this.simpleKey());
      this.ws();
      if (this.s[this.i] !== '.') return out;
      this.i++;
    }
  }

  simpleKey() {
    const c = this.s[this.i];
    if (c === '"') return this.basic();
    if (c === "'") return this.literal();
    const start = this.i;
    while (this.i < this.s.length && BARE.test(this.s[this.i])) this.i++;
    if (this.i === start) this.fail('expected a key');
    return this.s.slice(start, this.i);
  }

  value() {
    if (this.peek('"""')) return this.mlBasic();
    if (this.peek("'''")) return this.mlLiteral();
    const c = this.s[this.i];
    if (c === '"') return this.basic();
    if (c === "'") return this.literal();
    if (c === '[') return this.array();
    if (c === '{') return this.inline();
    const m = /^[^\s,\]}#]+(?: \d{2}:\d{2}[^\s,\]}#]*)?/.exec(this.s.slice(this.i));
    if (!m) this.fail('expected a value');
    this.i += m[0].length;
    const t = m[0];
    if (t === 'true') return true;
    if (t === 'false') return false;
    const num = t.replace(/_/g, '');
    if (/^[+-]?(\d+|0x[0-9a-fA-F]+|0o[0-7]+|0b[01]+)$/.test(num)) return Number(num.replace(/^\+/, ''));
    if (/^[+-]?(\d+(\.\d+)?([eE][+-]?\d+)?|inf|nan)$/.test(num)) return Number(num.replace(/inf/, 'Infinity'));
    return t; // date/time: keep the text
  }

  escape() {
    const e = this.s[this.i + 1];
    if (e in ESC) { this.i += 2; return ESC[e]; }
    const len = e === 'u' ? 4 : e === 'U' ? 8 : 0;
    if (!len) this.fail('bad escape');
    const hex = this.s.slice(this.i + 2, this.i + 2 + len);
    if (!/^[0-9a-fA-F]+$/.test(hex) || hex.length !== len) this.fail('bad unicode escape');
    this.i += 2 + len;
    return String.fromCodePoint(parseInt(hex, 16));
  }

  basic() {
    this.i++;
    let out = '';
    for (;;) {
      const c = this.s[this.i];
      if (c === undefined || c === '\n') this.fail('unterminated string');
      if (c === '"') { this.i++; return out; }
      if (c === '\\') { out += this.escape(); continue; }
      out += c;
      this.i++;
    }
  }

  literal() {
    const end = this.s.indexOf("'", this.i + 1);
    const nl = this.s.indexOf('\n', this.i + 1);
    if (end === -1 || (nl !== -1 && nl < end)) this.fail('unterminated string');
    const out = this.s.slice(this.i + 1, end);
    this.i = end + 1;
    return out;
  }

  skipFirstNewline() {
    if (this.peek('\r\n')) this.i += 2;
    else if (this.s[this.i] === '\n') this.i++;
  }

  mlBasic() {
    this.i += 3;
    this.skipFirstNewline();
    let out = '';
    for (;;) {
      if (this.i >= this.s.length) this.fail('unterminated string');
      if (this.peek('"""')) {
        let extra = 0;
        while (this.s[this.i + 3 + extra] === '"' && extra < 2) extra++;
        out += '"'.repeat(extra);
        this.i += 3 + extra;
        return out;
      }
      const c = this.s[this.i];
      if (c === '\\') {
        const rest = /^\\[ \t]*\r?\n[\s]*/.exec(this.s.slice(this.i));
        if (rest) { this.i += rest[0].length; continue; }
        out += this.escape();
        continue;
      }
      out += c;
      this.i++;
    }
  }

  mlLiteral() {
    this.i += 3;
    this.skipFirstNewline();
    const end = this.s.indexOf("'''", this.i);
    if (end === -1) this.fail('unterminated string');
    let stop = end;
    while (this.s[stop + 3] === "'" && stop - end < 2) stop++;
    const out = this.s.slice(this.i, stop);
    this.i = stop + 3;
    return out;
  }

  array() {
    this.i++;
    const out = [];
    for (;;) {
      this.blank();
      if (this.s[this.i] === ']') { this.i++; return out; }
      out.push(this.value());
      this.blank();
      if (this.s[this.i] === ',') { this.i++; continue; }
      if (this.s[this.i] === ']') { this.i++; return out; }
      this.fail('expected , or ]');
    }
  }

  inline() {
    this.i++;
    const out = {};
    for (;;) {
      this.blank(); // TOML 1.1 allows newlines here; Cargo accepts them
      if (this.s[this.i] === '}') { this.i++; return out; }
      const keys = this.key();
      this.expect('=');
      this.ws();
      assign(out, keys, this.value());
      this.blank();
      if (this.s[this.i] === ',') { this.i++; continue; }
      if (this.s[this.i] === '}') { this.i++; return out; }
      this.fail('expected , or }');
    }
  }
}

const isTable = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);

function descend(node, k) {
  let next = node[k];
  if (next === undefined) { next = {}; node[k] = next; }
  if (Array.isArray(next)) next = next.at(-1);
  if (!isTable(next)) throw new TomlError(`key ${k} is not a table`);
  return next;
}

function table(root, keys) {
  let node = root;
  for (const k of keys) node = descend(node, k);
  return node;
}

function arrayTable(root, keys) {
  let node = root;
  for (const k of keys.slice(0, -1)) node = descend(node, k);
  const last = keys.at(-1);
  if (node[last] === undefined) node[last] = [];
  if (!Array.isArray(node[last])) throw new TomlError(`key ${last} is not an array of tables`);
  const t = {};
  node[last].push(t);
  return t;
}

function assign(node, keys, value) {
  let cur = node;
  for (const k of keys.slice(0, -1)) cur = descend(cur, k);
  cur[keys.at(-1)] = value;
}

/** Parse TOML text. Returns null if the text is not valid TOML. */
export function parseToml(text) {
  try {
    return new Reader(String(text ?? '')).document();
  } catch (e) {
    if (e instanceof TomlError) return null;
    throw e;
  }
}
