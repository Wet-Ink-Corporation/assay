// The lint baseline and the ratchet.
//
// `.claude/temper-baseline.json` (committed; under the protected `.claude/`)
// records the accepted legacy debt as counts per (crate, lint), plus each
// crate's directory so a renamed crate keeps its counts. The ratchet fails when
// a finding sits on a line changed since the base (new code, or legacy code the
// change touched: CRF-08), or when a (crate, lint) count exceeds the baseline
// (non-local effects). Everything else is legacy debt and passes. Without a
// baseline the ratchet is strict: every finding is new.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { run, readText, writeAtomic, norm, BASELINE_FILE } from './project.mjs';
import { toolOf } from './findings.mjs';

export const BASELINE_PATH = BASELINE_FILE;

/** Lints that are never legacy: the forbid-override check stands in for E0453, a build error. */
export const ALWAYS_NEW = new Set(['scan:forbid-override']);

const git = (proj, args, env) => run('git', args, { cwd: proj.gitRoot, ...(env ? { env } : {}) });
const crateKey = (f) => f.crate ?? '(none)';
/** Findings that count as debt: approved lint exceptions (scanner `ask` hits) are not debt. */
const counted = (collection) => (collection.findings ?? []).filter((f) => !f.exception);

/** Parse baseline text: throws on a file that is not a temper baseline. The retired `files` table is dropped. */
export function parseBaseline(text) {
  let b;
  try { b = JSON.parse(text); } catch (e) { throw new Error(`${BASELINE_PATH} is not valid JSON (${e.message})`); }
  if (b?.temper_baseline !== 1 || typeof b.totals !== 'object' || b.totals === null) {
    throw new Error(`${BASELINE_PATH} is not a temper baseline (expected "temper_baseline": 1 and a "totals" table)`);
  }
  const { files: _files, ...rest } = b;
  return { ...rest, crates: b.crates ?? {} };
}

/** Read the baseline at `root`: null when absent; throws on a file that is not a temper baseline. */
export function readBaseline(root) {
  const t = readText(path.join(root, ...BASELINE_PATH.split('/')));
  return t === null ? null : parseBaseline(t);
}

/** A count table ({ key: { lint: n } }) with sorted keys and no zero or empty entries. */
function tidyTable(table) {
  const out = {};
  for (const k of Object.keys(table ?? {}).sort()) {
    const inner = {};
    for (const l of Object.keys(table[k] ?? {}).sort()) if (table[k][l] > 0) inner[l] = table[k][l];
    if (Object.keys(inner).length) out[k] = inner;
  }
  return out;
}

const HEADER = ['temper_baseline', 'created', 'updated', 'commit', 'toolchain', 'host'];

/** The baseline in its committed shape: header fields in a fixed order, then sorted tables, so diffs stay small. */
function tidy(b) {
  const out = {};
  for (const k of HEADER) if (b[k] !== undefined) out[k] = b[k];
  for (const k of Object.keys(b).filter((x) => !HEADER.includes(x) && !['crates', 'totals', 'files'].includes(x)).sort()) out[k] = b[k];
  out.crates = Object.fromEntries(Object.keys(b.crates ?? {}).sort().map((k) => [k, b.crates[k]]));
  out.totals = tidyTable(b.totals);
  return out;
}

export const renderBaseline = (baseline) => `${JSON.stringify(tidy(baseline), null, 2)}\n`;

/** Write the baseline: fixed header order, sorted keys, 2-space indent, trailing newline, no zero entries. Returns the path. */
export function writeBaseline(root, baseline) {
  const p = path.join(root, ...BASELINE_PATH.split('/'));
  writeAtomic(p, renderBaseline(baseline));
  return p;
}

function count(findings, key) {
  const out = {};
  for (const f of findings) {
    const k = key(f);
    out[k] ??= {};
    out[k][f.lint] = (out[k][f.lint] ?? 0) + 1;
  }
  return out;
}

/** A baseline recording every finding of `collection` (lint exceptions and never-legacy lints excluded). */
export function baselineFrom(collection, { created } = {}) {
  const now = new Date().toISOString();
  const found = counted(collection).filter((f) => !ALWAYS_NEW.has(f.lint));
  return tidy({
    temper_baseline: 1,
    created: created ?? now,
    updated: now,
    commit: collection.commit ?? null,
    toolchain: collection.toolchain ?? null,
    host: collection.host ?? null,
    crates: Object.fromEntries((collection.crates ?? []).map((c) => [c.name, c.dir])),
    totals: count(found, crateKey),
  });
}

const sum = (table) => Object.values(table ?? {}).reduce((n, lints) => n + Object.values(lints).reduce((a, b) => a + b, 0), 0);

/**
 * Crates renamed since the baseline: a baseline name that is gone, whose
 * directory now holds a crate the baseline does not name, or whose directory
 * moved (a file inside it was renamed to the same relative path, per `renames`)
 * to where such a crate is.
 * @param {Map<string,string>|(() => Map<string,string>)|null} renames new path -> old path
 *   (git-root relative), or a function computing it; only consulted when needed.
 * @returns {Map<string, string>} current name -> baseline name
 */
export function crateAliases(baseline, crates, renames = null) {
  const out = new Map();
  const known = baseline?.crates ?? {};
  const now = new Map((crates ?? []).map((c) => [c.name, c.dir]));
  const byDir = new Map((crates ?? []).map((c) => [c.dir, c.name]));
  let pairs = null;
  // Where a baseline crate dir went: a file renamed from inside it to the same relative path elsewhere.
  const movedTo = (dir) => {
    if (dir === '.') return null;
    pairs ??= [...((typeof renames === 'function' ? renames() : renames) ?? [])];
    for (const [to, from] of pairs) {
      if (!from.startsWith(`${dir}/`)) continue;
      const suffix = from.slice(dir.length);
      if (to.endsWith(suffix) && byDir.has(to.slice(0, -suffix.length))) return to.slice(0, -suffix.length);
    }
    return null;
  };
  for (const [old, dir] of Object.entries(known)) {
    if (now.has(old)) continue;
    let renamed = byDir.get(dir);
    if (!renamed || renamed in known) renamed = byDir.get(movedTo(dir));
    if (renamed && !(renamed in known) && ![...out.keys()].includes(renamed)) out.set(renamed, old);
  }
  return out;
}

/** Renames since the baseline was recorded (its `commit`), for crateAliases; empty without git. */
export function renamesSinceBaseline(proj, baseline, extra = null) {
  const out = new Map(extra ?? []);
  const from = baseline?.commit && proj?.gitRoot ? resolveCommit(proj, baseline.commit) : null;
  if (from) for (const [to, old] of renamedSince(proj, from)) if (!out.has(to)) out.set(to, old);
  return out;
}

/**
 * Lower every baseline count to the current count and drop zeros. Never raises
 * a count and never adds an entry. Lints whose source the collection did not
 * run (rustdoc with `gate.doc` off) keep their counts. With `proj`, a crate
 * renamed or moved since the baseline's commit keeps its entries.
 * @throws when the collection has compile errors (a crate that does not build reports no lints)
 */
export function tighten(baseline, collection, { proj } = {}) {
  if (collection.errors?.length) throw new Error(`cannot tighten against a workspace that does not build (${collection.errors.length} compile error(s))`);
  const sources = collection.sources ?? ['clippy', 'rustdoc', 'scan'];
  const alias = crateAliases(baseline, collection.crates, () => renamesSinceBaseline(proj, baseline));
  const now = count(counted(collection), (f) => alias.get(crateKey(f)) ?? crateKey(f));
  const totals = {};
  for (const [k, lints] of Object.entries(baseline.totals ?? {})) {
    for (const [lint, n] of Object.entries(lints)) {
      const v = Math.min(n, sources.includes(toolOf(lint)) ? (now[k]?.[lint] ?? 0) : n);
      if (v > 0) { totals[k] ??= {}; totals[k][lint] = v; }
    }
  }
  const changed = JSON.stringify(tidyTable(totals)) !== JSON.stringify(tidyTable(baseline.totals));
  return tidy({ ...baseline, totals, ...(changed ? { updated: new Date().toISOString() } : {}) });
}

/** (crate, lint) pairs whose count differs between two baselines: [{ crate, lint, from, to }]. */
export function diffTotals(before, after) {
  const out = [];
  const crates = new Set([...Object.keys(before?.totals ?? {}), ...Object.keys(after?.totals ?? {})]);
  for (const c of [...crates].sort()) {
    const lints = new Set([...Object.keys(before?.totals?.[c] ?? {}), ...Object.keys(after?.totals?.[c] ?? {})]);
    for (const l of [...lints].sort()) {
      const from = before?.totals?.[c]?.[l] ?? 0;
      const to = after?.totals?.[c]?.[l] ?? 0;
      if (from !== to) out.push({ crate: c, lint: l, from, to });
    }
  }
  return out;
}

// ------------------------------------------------------------------ changed lines

/** Unquote a path from a diff header (`"b/sp\303\251c.rs"` form), dropping the a/ or b/ prefix. */
function diffPath(raw) {
  let p = raw.replace(/\t$/, '');
  if (p.startsWith('"') && p.endsWith('"')) {
    const bytes = [];
    const body = p.slice(1, -1);
    for (let i = 0; i < body.length; i++) {
      const c = body[i];
      if (c !== '\\') { bytes.push(...Buffer.from(c, 'utf8')); continue; }
      const n = body[++i];
      if (/[0-7]/.test(n)) { bytes.push(parseInt(body.slice(i, i + 3), 8)); i += 2; continue; }
      bytes.push({ n: 10, t: 9, r: 13, '"': 34, '\\': 92, a: 7, b: 8, f: 12, v: 11 }[n] ?? n.charCodeAt(0));
    }
    p = Buffer.from(bytes).toString('utf8');
  }
  return norm(p.replace(/^[ab]\//, ''));
}

/**
 * Parse `git diff` output: file -> set of new-side line numbers of `+` body
 * lines. Hunk headers only position the walk, so context lines (a user's
 * `diff.interHunkContext`) never count as changed. Pure deletions and renames
 * without edits add nothing. Expects `a/` and `b/` prefixes (see DIFF_ARGS).
 * @returns {Map<string, Set<number>>} with `.renames`: new path -> old path
 */
export function parseDiff(text) {
  const out = new Map();
  out.renames = new Map();
  let file = null;
  let renameFrom = null;
  let oldLeft = 0;
  let newLeft = 0;
  let at = 0;
  for (const line of String(text).split(/\r?\n/)) {
    if (oldLeft > 0 || newLeft > 0) {
      if (line.startsWith('+')) { if (file) out.get(file).add(at); at++; newLeft--; continue; }
      if (line.startsWith('-')) { oldLeft--; continue; }
      if (line.startsWith(' ')) { at++; oldLeft--; newLeft--; continue; }
      if (line.startsWith('\\')) continue; // "\ No newline at end of file"
      oldLeft = 0; newLeft = 0;
    }
    if (line.startsWith('diff --git ')) { file = null; renameFrom = null; continue; }
    if (line.startsWith('rename from ')) { renameFrom = diffPath(`a/${line.slice(12)}`); continue; }
    if (line.startsWith('rename to ') && renameFrom) { out.renames.set(diffPath(`b/${line.slice(10)}`), renameFrom); renameFrom = null; continue; }
    if (line.startsWith('+++ ')) {
      const p = line.slice(4);
      file = p === '/dev/null' ? null : diffPath(p);
      if (file && !out.has(file)) out.set(file, new Set());
      continue;
    }
    const h = /^@@ -\d+(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/.exec(line);
    if (h) {
      oldLeft = h[1] === undefined ? 1 : Number(h[1]);
      at = Number(h[2]);
      newLeft = h[3] === undefined ? 1 : Number(h[3]);
    }
  }
  return out;
}

/**
 * `git diff` options that pin the output format against user configuration
 * (`diff.mnemonicPrefix`, `diff.noprefix`, `diff.srcPrefix`, `diff.interHunkContext`,
 * `core.quotepath`, external diff drivers, colour).
 */
export const DIFF_CONFIG = ['-c', 'diff.mnemonicPrefix=false', '-c', 'diff.noprefix=false', '-c', 'core.quotepath=false'];
export const DIFF_ARGS = ['--src-prefix=a/', '--dst-prefix=b/', '--inter-hunk-context=0', '--no-color', '--no-ext-diff', '--no-textconv', '-U0', '-M'];

/** Resolve a revision to a commit sha in this repository, or null. */
export function resolveCommit(proj, rev) {
  if (!proj?.gitRoot || !rev) return null;
  const r = git(proj, ['rev-parse', '--verify', '--quiet', `${rev}^{commit}`]);
  return r.code === 0 ? r.stdout.trim() || null : null;
}

/** The merge base of `base` and HEAD (or `base` itself when they share no history). */
export function mergeBase(proj, base) {
  const rev = resolveCommit(proj, base);
  if (!rev) return null;
  const mb = git(proj, ['merge-base', rev, 'HEAD']);
  return mb.code === 0 && mb.stdout.trim() ? mb.stdout.trim() : rev;
}

/** The files the ratchet diffs: Rust sources and manifests (crate renames), never target/ or .temper/. */
const DIFF_PATHSPECS = [':(glob)**/*.rs', ':(glob)**/Cargo.toml', ':(exclude).temper', ':(exclude)target'];

/**
 * Run `git diff <args> <from>` of the working tree against `from`, through a
 * temporary index: a copy of the real one plus the untracked `.rs` files and
 * manifests as intent-to-add entries. Untracked files then diff as new files
 * and unstaged renames and moves are detected like staged ones, while no file
 * content is written to .git/objects and the real index is never touched.
 * Only `.rs` files and Cargo.toml manifests are diffed.
 * @returns {string} the diff output
 */
function workingTreeDiff(proj, from, args) {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'temper-index-'));
  try {
    const index = path.join(tmp, 'index');
    const real = git(proj, ['rev-parse', '--git-path', 'index']).stdout.trim();
    const realPath = real ? path.resolve(proj.gitRoot, real) : '';
    const env = { GIT_INDEX_FILE: index };
    if (realPath && fs.existsSync(realPath)) fs.copyFileSync(realPath, index);
    else if (git(proj, ['read-tree', 'HEAD'], env).code !== 0) git(proj, ['read-tree', '--empty'], env);
    const others = git(proj, ['ls-files', '--others', '--exclude-standard', '-z', '--', '*.rs', '*Cargo.toml']).stdout.split('\0')
      .filter((f) => f && /(^|\/)(Cargo\.toml|[^/]+\.rs)$/.test(f) && !/^(?:target|\.temper)(?:\/|$)/.test(f));
    if (others.length) {
      const list = path.join(tmp, 'untracked');
      fs.writeFileSync(list, `${others.join('\0')}\0`);
      const add = git(proj, ['add', '--intent-to-add', '--pathspec-from-file', list, '--pathspec-file-nul'], { ...env, GIT_LITERAL_PATHSPECS: '1' });
      if (add.code !== 0) throw new Error(`git add -N (temporary index) failed: ${add.stderr.trim().split(/\r?\n/)[0] ?? ''}`);
    }
    const d = git(proj, [...DIFF_CONFIG, 'diff', ...args, from, '--', ...DIFF_PATHSPECS], env);
    if (d.code !== 0) throw new Error(`git diff against ${from.slice(0, 12)} failed: ${d.stderr.trim().split(/\r?\n/)[0] ?? ''}`);
    return d.stdout;
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
}

/**
 * Lines added or changed since the merge base of `base` and HEAD, in the
 * working tree (see workingTreeDiff), keyed by path relative to the git root.
 * Returns null without git or without a base (totals-only mode).
 * @throws when `base` does not name a commit
 * @returns {Map<string, Set<number>>|null} with `.base` (the merge base) and `.renames` (new path -> old path)
 */
export function changedLines(proj, base) {
  if (!proj?.gitRoot || !base) return null;
  const from = mergeBase(proj, base);
  if (!from) throw new Error(`base revision \`${base}\` is not a commit in this repository (shallow clone? fetch it, e.g. actions/checkout with fetch-depth: 0)`);
  const out = parseDiff(workingTreeDiff(proj, from, DIFF_ARGS));
  out.base = from;
  return out;
}

/**
 * Files renamed or moved since `base` (staged or not): new path -> path at `base`.
 * Empty without git, without a base, or when git fails.
 * @returns {Map<string, string>}
 */
export function renamedSince(proj, base) {
  const out = new Map();
  const from = proj?.gitRoot && base ? resolveCommit(proj, base) : null;
  if (!from) return out;
  let text = '';
  try { text = workingTreeDiff(proj, from, ['--name-status', '-z', '-M', '--no-color', '--no-ext-diff']); } catch { return out; }
  const parts = text.split('\0');
  for (let i = 0; i < parts.length; i++) {
    if (/^[RC]\d*$/.test(parts[i])) { out.set(norm(parts[i + 2]), norm(parts[i + 1])); i += 2; }
    else if (/^[AMDTU]$/.test(parts[i])) i += 1;
  }
  return out;
}

// The lines a finding occupies for the new-code check: its span when short, else
// its first line (a whole-item span, such as crate-level missing docs, would
// otherwise make every edit inside the item "touch" it).
const SHORT_SPAN = 5;
function touches(changed, f) {
  const lines = changed.get(f.file);
  if (!lines) return false;
  if (lines === 'all') return true;
  const end = f.end_line && f.end_line - f.line < SHORT_SPAN ? f.end_line : f.line;
  for (let l = f.line; l <= end; l++) if (lines.has(l)) return true;
  return false;
}

/**
 * Counts the baseline had at the merge base that are higher now: a raise that
 * a human approved still merges, but CI and the report say so.
 * @returns {{ raised: object[], note: string|null }}
 */
function raisedSince(proj, mb, baseline) {
  const at = git(proj, ['show', `${mb}:${BASELINE_PATH}`]);
  let before = { totals: {}, crates: {} };
  let note = null;
  if (at.code === 0) {
    try { before = parseBaseline(at.stdout); } catch (e) { note = `the baseline at ${mb.slice(0, 12)} is unreadable (${e.message}); every count is compared with zero`; }
  } else note = `the baseline is new since ${mb.slice(0, 12)}: recording legacy debt is a human decision (H-12)`;
  // a crate renamed between the two baselines keeps its counts
  const alias = crateAliases(before, Object.entries(baseline.crates ?? {}).map(([name, dir]) => ({ name, dir })));
  const renamed = { totals: {} };
  for (const [c, lints] of Object.entries(before.totals ?? {})) {
    const now = [...alias].find(([, old]) => old === c)?.[0] ?? c;
    renamed.totals[now] = lints;
  }
  return { raised: diffTotals(renamed, baseline).filter((d) => d.to > d.from), note };
}

/**
 * The ratchet: the one definition of new, legacy and fixed findings.
 * With a baseline it fails on compile errors, on a finding on a line changed
 * since `base`, on a (crate, lint) count above the baseline, and on a
 * never-legacy lint (forbid-override). Without one (`baseline` null) it is
 * strict: every finding is new.
 * @param {object} collection from collectFindings()
 * @param {object|null} baseline from readBaseline()
 * @param {{ proj?: object, base?: string|null, changed?: Map|null, sources?: string[] }} opts
 *   `changed` overrides the git diff (tests, or a diff computed once per gate).
 *   `sources` narrows the comparison to findings from those tools (default: every source collected).
 *   With `proj` and a resolvable `base`, the baseline at the merge base is compared too (`raised`;
 *   `checkRaised: false` skips it), and crates renamed or moved since the baseline keep their counts.
 * @returns {{ ok: boolean, mode: 'strict'|'diff+totals'|'totals', base: string|null, new_code: object[],
 *   increases: object[], fixed: object[], fixed_count: number, legacy_total: number, baseline_total: number,
 *   raised: object[], errors: object[], notes: string[] }}
 */
export function ratchet(collection, baseline, { proj, base, changed, sources, checkRaised = true } = {}) {
  const notes = [];
  const collected = collection.sources ?? ['clippy', 'rustdoc', 'scan'];
  const srcs = (sources ?? collected).filter((s) => collected.includes(s));
  const errors = collection.errors ?? [];
  const found = counted(collection).filter((f) => srcs.includes(f.tool ?? toolOf(f.lint)) || ALWAYS_NEW.has(f.lint));
  if (!baseline) {
    return {
      ok: errors.length === 0 && found.length === 0, mode: 'strict', base: null, new_code: found, increases: [], fixed: [],
      fixed_count: 0, legacy_total: 0, baseline_total: 0, raised: [], errors,
      notes: ['no baseline: zero findings required'],
    };
  }
  let diff = changed;
  if (diff === undefined) {
    try { diff = changedLines(proj, base); } catch (e) { diff = null; notes.push(`${e.message}; only the totals were compared`); }
  }
  const alias = crateAliases(baseline, collection.crates, () => renamesSinceBaseline(proj, baseline, diff?.renames));
  for (const [now, old] of alias) {
    const same = baseline.crates?.[old] === (collection.crates ?? []).find((c) => c.name === now)?.dir;
    notes.push(`crate \`${old}\` is now \`${now}\` (${same ? 'same directory' : 'moved'}): its baseline counts carry over`);
  }
  const keyOf = (f) => alias.get(crateKey(f)) ?? crateKey(f);
  const newCode = found.filter((f) => ALWAYS_NEW.has(f.lint) || (diff && touches(diff, f)));
  const now = count(found.filter((f) => !ALWAYS_NEW.has(f.lint)), keyOf);
  const was = {};
  for (const [c, lints] of Object.entries(baseline.totals ?? {})) {
    for (const [l, n] of Object.entries(lints)) if (srcs.includes(toolOf(l))) { was[c] ??= {}; was[c][l] = n; }
  }
  const increases = [];
  const fixed = [];
  for (const c of [...new Set([...Object.keys(now), ...Object.keys(was)])].sort()) {
    for (const l of [...new Set([...Object.keys(now[c] ?? {}), ...Object.keys(was[c] ?? {})])].sort()) {
      const cur = now[c]?.[l] ?? 0;
      const b = was[c]?.[l] ?? 0;
      if (cur > b) increases.push({ crate: c, lint: l, baseline: b, current: cur });
      else if (cur < b) fixed.push({ crate: c, lint: l, baseline: b, current: cur });
    }
  }
  if (baseline.toolchain && collection.toolchain && baseline.toolchain !== collection.toolchain) {
    notes.push(`the baseline was recorded on Rust ${baseline.toolchain} and this run used ${collection.toolchain}: lints new in this toolchain count as increases; recording them (temper baseline --update) is a human decision (H-12)`);
  }
  if (baseline.host && collection.host && baseline.host !== collection.host) {
    notes.push(`the baseline was recorded on ${baseline.host} and this run is on ${collection.host}: code under cfg(target_os/unix/windows) lints differently; record and tighten the baseline on the CI platform`);
  }
  let raised = [];
  if (checkRaised && proj?.gitRoot && diff?.base) {
    const r = raisedSince(proj, diff.base, baseline);
    raised = r.raised;
    if (raised.length && r.note) notes.push(r.note);
  }
  return {
    ok: errors.length === 0 && newCode.length === 0 && increases.length === 0,
    mode: diff ? 'diff+totals' : 'totals',
    base: diff?.base ?? (diff ? base ?? null : null),
    new_code: newCode,
    increases,
    fixed,
    fixed_count: fixed.reduce((n, x) => n + x.baseline - x.current, 0),
    legacy_total: found.length - newCode.length,
    baseline_total: sum(was),
    raised,
    errors,
    notes,
  };
}

/** One line of accepted debt. `--tighten` is suggested only when it can help: a passing ratchet with fixes. */
export function debtLine(res) {
  if (res.mode === 'strict') return `no baseline: zero findings required (${res.new_code.length} found)`;
  const fixed = res.fixed_count && res.ok ? `; ${res.fixed_count} fixed since the baseline: run temper baseline --tighten` : '';
  return `legacy: ${res.legacy_total} finding(s) (baseline ${res.baseline_total})${fixed}`;
}

/** Human-readable ratchet result: errors, new-code findings (at most `max`), increases, a raised baseline, notes, then the debt line. */
export function renderRatchet(res, { max = 30 } = {}) {
  const out = [];
  const nested = res.errors.find((e) => e.code === 'workspace-below-git-root');
  if (nested) out.push(nested.message);
  else if (res.errors.length) {
    out.push(`${res.errors.length} compile error(s); the ratchet needs a workspace that builds:`);
    for (const e of res.errors.slice(0, 12)) out.push(`  ${e.file ? `${e.file}:${e.line} ` : ''}${e.code ? `${e.code} ` : ''}${e.message}`);
    if (res.errors.length > 12) out.push(`  ... ${res.errors.length - 12} more`);
  }
  if (res.new_code.length) {
    out.push(res.mode === 'strict'
      ? `${res.new_code.length} finding(s); without a baseline every warning fails:`
      : `${res.new_code.length} finding(s) on lines changed since ${res.base ? res.base.slice(0, 12) : 'the base'} (new code, or legacy code the change touched: fix it, CRF-08):`);
    for (const f of res.new_code.slice(0, max)) out.push(`  ${f.file}:${f.line} ${f.lint} ${f.message}${f.rules?.length ? ` [${f.rules.join(', ')}]` : ''}`);
    if (res.new_code.length > max) out.push(`  ... ${res.new_code.length - max} more`);
  }
  // Increases the new-code findings already explain are not repeated.
  const fresh = {};
  for (const f of res.new_code) { const k = `${f.crate ?? '(none)'}\0${f.lint}`; fresh[k] = (fresh[k] ?? 0) + 1; }
  const beyond = res.increases.filter((x) => x.current - (fresh[`${x.crate}\0${x.lint}`] ?? 0) > x.baseline);
  if (beyond.length) {
    out.push(`${beyond.length} count(s) above the baseline on lines this change did not touch (a change can make unchanged code warn):`);
    for (const x of beyond.slice(0, max)) out.push(`  ${x.crate} ${x.lint}: baseline ${x.baseline}, now ${x.current}`);
    if (beyond.length > max) out.push(`  ... ${beyond.length - max} more`);
  }
  if (res.raised?.length) {
    out.push(`BASELINE RAISED since ${res.base ? res.base.slice(0, 12) : 'the base'}: ${res.raised.length} count(s) higher in ${BASELINE_PATH} (accepted debt grew; a human decision, H-12):`);
    for (const x of res.raised.slice(0, max)) out.push(`  ${x.crate} ${x.lint}: ${x.from} -> ${x.to}`);
    if (res.raised.length > max) out.push(`  ... ${res.raised.length - max} more`);
  }
  for (const n of res.notes ?? []) if (res.mode !== 'strict') out.push(`note: ${n}`);
  if (res.mode === 'totals' && !(res.notes ?? []).some((n) => /totals/.test(n))) out.push('note: no base revision: only the totals were compared');
  out.push(debtLine(res));
  return out.join('\n');
}

export const baselineExists = (root) => fs.existsSync(path.join(root, ...BASELINE_PATH.split('/')));
