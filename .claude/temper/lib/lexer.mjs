// Minimal Rust lexer: masks comments and string/char literal contents so that
// pattern rules only ever see real code. Offsets and newlines are preserved,
// so a match offset in the masked text maps 1:1 to the original source.

const isIdent = (ch) => ch !== undefined && /[A-Za-z0-9_]/.test(ch);

/**
 * @param {string} src
 * @returns {{ code: string, comments: {start:number,end:number,text:string,doc:boolean}[] }}
 */
export function maskRust(src) {
  const out = src.split('');
  const comments = [];
  const blank = (from, to) => {
    for (let k = from; k < to; k++) if (out[k] !== '\n' && out[k] !== '\r') out[k] = ' ';
  };
  let i = 0;
  const n = src.length;
  while (i < n) {
    const c = src[i];
    const d = src[i + 1];
    // line comment
    if (c === '/' && d === '/') {
      let j = src.indexOf('\n', i);
      if (j === -1) j = n;
      const text = src.slice(i, j);
      comments.push({ start: i, end: j, text, doc: /^\/\/[\/!]/.test(text) && !/^\/\/\/\//.test(text) });
      blank(i, j);
      i = j;
      continue;
    }
    // block comment (nested)
    if (c === '/' && d === '*') {
      let depth = 1;
      let j = i + 2;
      while (j < n && depth > 0) {
        if (src[j] === '/' && src[j + 1] === '*') { depth++; j += 2; continue; }
        if (src[j] === '*' && src[j + 1] === '/') { depth--; j += 2; continue; }
        j++;
      }
      const text = src.slice(i, j);
      comments.push({ start: i, end: j, text, doc: /^\/\*[*!]/.test(text) && !/^\/\*\*\//.test(text) });
      blank(i, j);
      i = j;
      continue;
    }
    // raw strings: r"..", r#".."#, br#"..", cr#".."
    if ((c === 'r' || ((c === 'b' || c === 'c') && d === 'r')) && !isIdent(src[i - 1])) {
      let j = c === 'r' ? i + 1 : i + 2;
      let hashes = 0;
      while (src[j] === '#') { hashes++; j++; }
      if (src[j] === '"') {
        const close = '"' + '#'.repeat(hashes);
        const end = src.indexOf(close, j + 1);
        const stop = end === -1 ? n : end;
        blank(j + 1, stop);
        i = end === -1 ? n : end + close.length;
        continue;
      }
    }
    // normal / byte / c strings
    if (c === '"') {
      let j = i + 1;
      while (j < n && src[j] !== '"') {
        if (src[j] === '\\') j += 2; else j++;
      }
      blank(i + 1, Math.min(j, n));
      i = j + 1;
      continue;
    }
    // char literal vs lifetime / label
    if (c === "'") {
      if (d === '\\') {
        let j = i + 3; // skip the escaped character, which may itself be a quote: '\''
        while (j < n && src[j] !== "'" && src[j] !== '\n') j++;
        if (src[j] === "'") { blank(i + 1, j); i = j + 1; continue; }
      } else if (d !== undefined) {
        const cp = src.codePointAt(i + 1);
        const len = cp > 0xffff ? 2 : 1;
        if (src[i + 1 + len] === "'" && d !== "'") { blank(i + 1, i + 1 + len); i = i + 2 + len; continue; }
      }
      i++; // lifetime or label: leave as code
      continue;
    }
    i++;
  }
  return { code: out.join(''), comments };
}

/** @param {string} text */
export function lineIndex(text) {
  const starts = [0];
  for (let k = 0; k < text.length; k++) if (text[k] === '\n') starts.push(k + 1);
  return {
    starts,
    lineOf(offset) {
      let lo = 0, hi = starts.length - 1;
      while (lo < hi) {
        const mid = (lo + hi + 1) >> 1;
        if (starts[mid] <= offset) lo = mid; else hi = mid - 1;
      }
      return lo + 1;
    },
    lineText(line, source) {
      const s = starts[line - 1] ?? 0;
      let e = source.indexOf('\n', s);
      if (e === -1) e = source.length;
      return source.slice(s, e);
    },
  };
}

/** Find the offset of the matching close brace for the `{` at `open`. */
export function matchBrace(code, open) {
  return matchPair(code, open, '{', '}');
}

/** Offset of the bracket closing the `o` at `open` (end of text if unbalanced). */
export function matchPair(code, open, o, c) {
  let depth = 0;
  for (let k = open; k < code.length; k++) {
    if (code[k] === o) depth++;
    else if (code[k] === c) { depth--; if (depth === 0) return k; }
  }
  return code.length - 1;
}

/**
 * Offset of the `>` closing the generic list opened at `open` (`->` is not a
 * closer), or -1 if a `{` or `;` comes first.
 */
export function matchAngle(code, open) {
  let depth = 0;
  for (let k = open; k < code.length; k++) {
    const ch = code[k];
    if (ch === '<') depth++;
    else if (ch === '>' && code[k - 1] !== '-') { depth--; if (depth === 0) return k; }
    else if (ch === '{' || ch === ';') return -1;
  }
  return -1;
}

/** Split `s` on `sep` at bracket depth 0 (`<>`, `()`, `[]`, `{}`; `->` is not a bracket). */
export function splitTop(s, sep = ',') {
  const out = [];
  let depth = 0;
  let from = 0;
  for (let k = 0; k < s.length; k++) {
    const ch = s[k];
    if ('<([{'.includes(ch)) depth++;
    else if (')]}'.includes(ch) || (ch === '>' && s[k - 1] !== '-')) depth--;
    else if (ch === sep && depth === 0) { out.push(s.slice(from, k)); from = k + 1; }
  }
  out.push(s.slice(from));
  return out;
}

/**
 * Offset of the `;` that ends the statement or item starting at `from`, or of
 * the `}` closing the enclosing block when the statement is a tail expression.
 */
export function statementEnd(code, from) {
  let depth = 0;
  for (let k = from; k < code.length; k++) {
    const ch = code[k];
    if (ch === '(' || ch === '[' || ch === '{') depth++;
    else if (ch === ')' || ch === ']' || ch === '}') { depth--; if (depth < 0) return k; }
    else if (ch === ';' && depth === 0) return k;
  }
  return code.length;
}

/**
 * Regions evaluated at compile time: inline `const { .. }` blocks and
 * `const NAME: T = ..;` items (not const generic parameters, not `const fn`).
 * @returns {[number, number][]}
 */
export function constRegions(code) {
  const regions = [];
  const block = /\bconst\s*\{/g;
  let m;
  while ((m = block.exec(code))) regions.push([m.index, matchBrace(code, code.indexOf('{', m.index))]);
  const item = /\bconst\s+(?:_|[A-Za-z_]\w*)\s*:/g;
  while ((m = item.exec(code))) {
    const prev = code.slice(0, m.index).trimEnd().at(-1);
    if (prev === '<' || prev === ',') continue; // const generic parameter
    regions.push([m.index, statementEnd(code, m.index)]);
  }
  return regions;
}

/** Bodies of trait impls (`impl Trait for Type { .. }`), whose method names the trait fixes. */
export function traitImplRegions(code) {
  const regions = [];
  const re = /\bimpl\b/g;
  let m;
  while ((m = re.exec(code))) {
    let depth = 0;
    let open = -1;
    for (let k = m.index + 4; k < code.length; k++) {
      const ch = code[k];
      if (ch === '<') depth++;
      else if (ch === '>' && code[k - 1] !== '-') depth--;
      else if ((ch === '{' || ch === ';' || ch === ')') && depth <= 0) { open = ch === '{' ? k : -1; break; }
    }
    if (open === -1) continue;
    if (/\bfor\b(?!\s*<)/.test(code.slice(m.index, open))) regions.push([m.index, matchBrace(code, open)]);
  }
  return regions;
}

/**
 * Regions of masked code that are test-only: `#[cfg(test)]` items and
 * `#[test]`-style functions (including `#[tokio::test]`, `#[rstest]`) and `proptest!` blocks.
 * @returns {[number, number][]}
 */
export function testRegions(code) {
  const regions = [];
  const attr = /#\s*\[\s*(cfg\s*\(([^\]]*)\)|(?:[A-Za-z_][\w]*\s*::\s*)*(?:test|rstest|test_case)\b[^\]]*)\s*\]/g;
  let m;
  while ((m = attr.exec(code))) {
    if (m[2] !== undefined) {
      const inner = m[2];
      if (!/\btest\b/.test(inner) || /\bnot\s*\(\s*test\b/.test(inner)) continue;
    }
    // The item that follows: its body (the next `{` at depth 0), or up to its `;`
    // for bodiless items such as `#[cfg(test)] static A: T = T;` or `mod tests;`.
    let depth = 0;
    let end = -1;
    for (let k = attr.lastIndex; k < code.length; k++) {
      const ch = code[k];
      if (ch === '(' || ch === '[') depth++;
      else if (ch === ')' || ch === ']') depth--;
      else if (ch === '{' && depth <= 0) { end = matchBrace(code, k); break; }
      else if (ch === ';' && depth <= 0) { end = k; break; }
    }
    if (end === -1) continue;
    regions.push([m.index, end]);
  }
  const pt = /\bproptest\s*!\s*\{/g;
  while ((m = pt.exec(code))) {
    const open = code.indexOf('{', m.index);
    regions.push([m.index, matchBrace(code, open)]);
  }
  return regions;
}

export const inRegions = (regions, offset) => regions.some(([a, b]) => offset >= a && offset <= b);

/**
 * Count `#[ignore]` attributes, reasoned or not, including `cfg_attr(.., ignore)`.
 * `cfg_attr(miri, ignore = ..)` is excluded: TST-17 requires it for tests Miri cannot run.
 */
export function countIgnored(code) {
  let n = 0;
  const re = /#\s*\[\s*(?:ignore\b|cfg_attr\s*\(([^\]]*?),\s*ignore\b)/g;
  let m;
  while ((m = re.exec(code))) {
    if (m[1] === undefined) n++;
    else if (topLevelCfgAttrArg(m[0]) && m[1].trim() !== 'miri') n++;
  }
  return n;
}

/**
 * For matched text `#[cfg_attr(<pred>, .., ignore`: is `ignore` a top-level
 * argument of `cfg_attr`, not nested in another attribute (`educe(Debug, ignore)`)?
 */
export function topLevelCfgAttrArg(text) {
  const at = text.indexOf('cfg_attr');
  if (at < 0) return true;
  let depth = 0;
  for (const ch of text.slice(text.indexOf('(', at) + 1)) {
    if (ch === '(') depth++;
    else if (ch === ')') depth--;
  }
  return depth === 0;
}

/** Count test functions (used to detect test deletion, H-12). */
export function countTests(code) {
  const attrs = code.match(/#\s*\[\s*(?:[A-Za-z_][\w]*\s*::\s*)*(?:test|rstest|test_case)\b[^\]]*\]/g) ?? [];
  const props = code.match(/\bproptest\s*!\s*\{/g) ?? [];
  // each `fn` inside a proptest! block is a test; approximate by counting `fn` in those blocks
  let propFns = 0;
  const re = /\bproptest\s*!\s*\{/g;
  let m;
  while ((m = re.exec(code))) {
    const open = code.indexOf('{', m.index);
    const close = matchBrace(code, open);
    propFns += (code.slice(open, close).match(/\bfn\s+\w+/g) ?? []).length;
  }
  return attrs.length + (propFns || props.length);
}
