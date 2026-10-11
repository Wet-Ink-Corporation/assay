// Findings: every lint diagnostic in the workspace, collected in one pass.
//
//   clippy   cargo clippy --workspace --all-targets --message-format=json -- --cap-lints warn
//   rustdoc  cargo doc --workspace --no-deps --message-format=json, RUSTDOCFLAGS=--cap-lints warn
//   scan     the escape-hatch scanner over every workspace .rs file (hard and ask hits)
//
// `--cap-lints warn` after `--` reaches clippy-driver for workspace crates only,
// so every lint reports without failing the build and dependents still get
// checked; RUSTFLAGS would invalidate the dependency cache. Each finding is
// mapped to the rules that cite its lint through rulemap.json (generated from
// rules.md by tests/gen_rulemap.py; nothing parses rules.md at run time).
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { StringDecoder } from 'node:string_decoder';
import { run, readText, relTo, norm, headRev, globMatch, writeAtomic, findUp } from './project.mjs';
import { scanSource, classifyPath, isBuildScript } from './scan.mjs';
import { maskRust, matchPair, splitTop } from './lexer.mjs';
import { parseToml } from './toml.mjs';
import { inheritsLints } from './manifest.mjs';

export const SOURCES = ['clippy', 'rustdoc', 'scan'];
const LEVEL_RANK = { MUST: 3, SHOULD: 2, CONSIDER: 1 };

function loadRulemap() {
  try {
    const t = fs.readFileSync(new URL('./rulemap.json', import.meta.url), 'utf8');
    const m = JSON.parse(t);
    return { lints: m.lints ?? {}, scan: m.scan ?? {}, rules: m.rules ?? {} };
  } catch {
    return { lints: {}, scan: {}, rules: {} };
  }
}

let rulemap = null;
/** rulemap.json: lint -> rule IDs, scanner key -> rule IDs, rule ID -> { level, family, title }. Read on first use. */
export function ruleMap() {
  rulemap ??= loadRulemap();
  return rulemap;
}

/** The source that reports a lint: `scan:*` the scanner, `rustdoc::*` rustdoc, anything else clippy (rustc lints included). */
export function toolOf(lint) {
  if (String(lint).startsWith('scan:')) return 'scan';
  if (String(lint).startsWith('rustdoc::')) return 'rustdoc';
  return 'clippy';
}

/** The highest level among rule IDs (MUST > SHOULD > CONSIDER), lower-cased; `unmapped` for none. */
export function severityOf(rules, map = ruleMap()) {
  let severity = 'unmapped';
  let rank = 0;
  for (const id of rules) {
    const level = map.rules[id]?.level;
    if ((LEVEL_RANK[level] ?? 0) > rank) { rank = LEVEL_RANK[level]; severity = level.toLowerCase(); }
  }
  return severity;
}

/** Rules citing a lint (`scan:<key>` for scanner hits), and the highest level among them. */
export function rulesFor(lint, map = ruleMap()) {
  const rules = lint.startsWith('scan:') ? (map.scan[lint.slice(5)] ?? []) : (map.lints[lint] ?? []);
  return { rules: [...rules], severity: severityOf(rules, map) };
}

const isLintCode = (code) => typeof code === 'string' && code !== '' && !/^E\d{4}$/.test(code);
const inProject = (rel) => !rel.startsWith('../') && rel !== '..' && !path.isAbsolute(rel);

/**
 * Where a diagnostic is reported. A diagnostic raised inside a macro expansion
 * (a local `macro_rules!` body, a derive) is attributed to the outermost call
 * site inside the project, so each invocation is its own finding on its own
 * line; `expansion` keeps the chain (primary span first, then each call site)
 * as `file:line:col` strings.
 */
function locate(spans, workspaceRoot, projectRoot) {
  const primary = (spans ?? []).find((s) => s.is_primary) ?? (spans ?? [])[0];
  if (!primary) return null;
  const chain = [];
  for (let s = primary; s && chain.length < 64; s = s.expansion?.span) chain.push(s);
  const rel = (s) => relTo(projectRoot, path.resolve(workspaceRoot, s.file_name));
  const where = (s) => { const r = rel(s); return inProject(r) ? r : norm(path.resolve(workspaceRoot, s.file_name)); };
  const pick = [...chain].reverse().find((s) => inProject(rel(s))) ?? primary;
  return {
    file: where(pick), line: pick.line_start, end_line: pick.line_end, column: pick.column_start,
    ...(chain.length > 1 ? { expansion: chain.map((s) => `${where(s)}:${s.line_start}:${s.column_start}`) } : {}),
  };
}

/**
 * An incremental parser for `cargo --message-format=json` lines. Lint
 * diagnostics become findings; error-level diagnostics that are not lints
 * (E-codes, syntax errors) become errors; notes and code-less warnings (which
 * `-D warnings` does not deny either) are dropped. `file` is relative to
 * `projectRoot` with forward slashes.
 */
function messageParser({ tool, workspaceRoot, projectRoot, map = ruleMap(), onRendered }) {
  const findings = [];
  const errors = [];
  return {
    line(line) {
      if (!line.startsWith('{')) return;
      let m;
      try { m = JSON.parse(line); } catch { return; }
      if (m.reason !== 'compiler-message' || !m.message) return;
      const d = m.message;
      if (onRendered && d.rendered) onRendered(d.rendered);
      const code = d.code?.code ?? null;
      const at = locate(d.spans, workspaceRoot, projectRoot);
      if (isLintCode(code)) {
        if (!at) return; // a lint with no location (crate-level flags) cannot be ratcheted
        findings.push({ tool, lint: code, crate: null, ...at, message: d.message, ...rulesFor(code, map) });
      } else if (/^error/.test(d.level ?? '')) {
        errors.push({ tool, code, file: at?.file ?? null, line: at?.line ?? null, message: d.message, target: m.target?.name ?? null });
      }
    },
    result() {
      return { findings, errors: dedupe(errors, (e) => `${e.code}\0${e.file}\0${e.line}\0${e.message}`) };
    },
  };
}

/**
 * Parse `cargo --message-format=json` output held in memory (tests, small runs).
 * @returns {{ findings: object[], errors: object[] }}
 */
export function parseCargoMessages(text, opts) {
  const p = messageParser(opts);
  for (const line of String(text).split(/\r?\n/)) p.line(line);
  return p.result();
}

/** Call `fn` for each line of a file, reading it in 1 MiB chunks (bounded memory for any output size). */
function forEachLine(file, fn) {
  const fd = fs.openSync(file, 'r');
  const dec = new StringDecoder('utf8');
  const buf = Buffer.allocUnsafe(1 << 20);
  let rest = '';
  try {
    for (;;) {
      const n = fs.readSync(fd, buf, 0, buf.length, null);
      if (n <= 0) break;
      const lines = (rest + dec.write(buf.subarray(0, n))).split('\n');
      rest = lines.pop();
      for (const l of lines) fn(l.replace(/\r$/, ''));
    }
    rest += dec.end();
    if (rest) fn(rest.replace(/\r$/, ''));
  } finally { fs.closeSync(fd); }
}

function dedupe(items, key) {
  const seen = new Set();
  return items.filter((x) => { const k = key(x); if (seen.has(k)) return false; seen.add(k); return true; });
}

/**
 * Drop repeats of one diagnostic: the same lint on the same span through the
 * same macro call sites, reported by the lib and the lib-test target, or by
 * clippy and rustdoc (`missing_docs`). The first report wins, so merge clippy
 * before rustdoc.
 */
export function dedupeFindings(findings) {
  const scan = findings.filter((f) => f.tool === 'scan'); // one hit per match: never repeated
  const cargo = dedupe(findings.filter((f) => f.tool !== 'scan'), (f) => `${f.lint}\0${f.file}\0${f.line}\0${f.column ?? ''}\0${f.message}\0${(f.expansion ?? []).join('|')}`);
  return [...cargo, ...scan];
}

/** Workspace members from `cargo metadata --no-deps` output: { name, dir, lints_inherited, charter }. */
export function crateTable(meta, projectRoot) {
  const members = new Set(meta.workspace_members ?? []);
  return (meta.packages ?? []).filter((p) => !members.size || members.has(p.id)).map((p) => {
    const dirAbs = path.dirname(p.manifest_path);
    const rel = relTo(projectRoot, dirAbs);
    const manifest = parseToml(readText(p.manifest_path));
    return {
      name: p.name,
      dir: rel === '' ? '.' : rel,
      lints_inherited: inheritsLints(manifest),
      charter: fs.existsSync(path.join(dirAbs, 'AGENTS.md')),
    };
  }).sort((a, b) => a.name.localeCompare(b.name));
}

/** The crate owning a project-relative file: the longest matching crate dir. */
export function crateFor(file, crates) {
  let best = null;
  for (const c of crates) {
    const hit = c.dir === '.' ? !file.startsWith('../') && !path.isAbsolute(file) : file === c.dir || file.startsWith(`${c.dir}/`);
    if (hit && (!best || (best.dir === '.' ? 0 : best.dir.length) < (c.dir === '.' ? 0 : c.dir.length))) best = c;
  }
  return best;
}

const sortFindings = (list) => list.sort((a, b) => a.file.localeCompare(b.file) || a.line - b.line || a.lint.localeCompare(b.lint));

/** Every .rs file of the project: git's view (tracked and untracked, ignores respected), else a walk. */
function rustFiles(proj) {
  if (proj.gitRoot) {
    const r = run('git', ['ls-files', '--cached', '--others', '--exclude-standard', '-z', '--', '*.rs'], { cwd: proj.gitRoot });
    if (r.code === 0) {
      return [...new Set(r.stdout.split('\0').filter(Boolean))]
        .map((f) => path.join(proj.gitRoot, f))
        .filter((f) => fs.existsSync(f));
    }
  }
  const out = [];
  const walk = (d) => {
    let es = [];
    try { es = fs.readdirSync(d, { withFileTypes: true }); } catch { return; }
    for (const e of es) {
      if (['target', '.git', 'node_modules', '.temper'].includes(e.name)) continue;
      const p = path.join(d, e.name);
      if (e.isDirectory()) walk(p); else if (e.name.endsWith('.rs')) out.push(p);
    }
  };
  walk(proj.root);
  return out;
}

const levelOf = (v) => (typeof v === 'string' ? v : v?.level);

/** Lints set to `forbid` in a `[lints]`-shaped table ({ rust: {..}, clippy: {..}, rustdoc: {..} }). */
function forbidden(table) {
  const out = new Set();
  for (const [tool, lints] of Object.entries(table ?? {})) {
    if (!lints || typeof lints !== 'object') continue;
    for (const [name, v] of Object.entries(lints)) {
      if (levelOf(v) === 'forbid') out.add(tool === 'rust' ? name.replace(/-/g, '_') : `${tool}::${name.replace(/-/g, '_')}`);
    }
  }
  return out;
}

/**
 * The lints each member forbids: the workspace `[workspace.lints]` table for a
 * member with `[lints] workspace = true`, else the member's own `[lints]`.
 * @returns {Map<string, Set<string>>} crate name -> forbidden lint names
 */
export function forbiddenLints(proj, crates, workspaceRoot = proj.root) {
  const root = parseToml(readText(path.join(workspaceRoot, 'Cargo.toml')));
  const ws = forbidden(root?.workspace?.lints);
  const out = new Map();
  for (const c of crates) {
    const m = parseToml(readText(path.join(proj.root, c.dir, 'Cargo.toml')));
    if (inheritsLints(m)) out.set(c.name, ws);
    else { const { workspace: _w, ...own } = m?.lints ?? {}; out.set(c.name, forbidden(own)); }
  }
  return out;
}

/** Lint names in `#![forbid(..)]` inner attributes of masked source. */
function innerForbids(code) {
  const out = new Set();
  const re = /#\s*!\s*\[\s*forbid\s*\(/g;
  let m;
  while ((m = re.exec(code))) {
    const open = m.index + m[0].length - 1;
    for (const item of splitTop(code.slice(open + 1, matchPair(code, open, '(', ')')))) {
      const lint = item.replace(/\s+/g, '');
      if (/^[a-z_][a-z0-9_]*(?:::[a-z_][a-z0-9_]*)?$/.test(lint)) out.add(lint);
    }
  }
  return out;
}

const MOD_DECL = /(?:^|[;{}\]\s])(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+(?:r#)?([A-Za-z_]\w*)\s*;/g;

/** Target root files (crate roots rustc compiles) per member, relative to the project root, from cargo metadata. */
export function targetRoots(meta, projectRoot) {
  const members = new Set(meta.workspace_members ?? []);
  const out = new Map();
  for (const p of meta.packages ?? []) {
    if (members.size && !members.has(p.id)) continue;
    out.set(p.name, (p.targets ?? []).map((tg) => relTo(projectRoot, tg.src_path)).filter(inProject));
  }
  return out;
}

/** Conventional target roots of a crate dir (when cargo metadata's targets are not at hand). */
function conventionalRoots(proj, dir) {
  const rel = (...p) => path.posix.join(dir === '.' ? '' : dir, ...p);
  const out = [rel('src/lib.rs'), rel('src/main.rs'), rel('build.rs')];
  const list = (sub) => { try { return fs.readdirSync(path.join(proj.root, dir, sub), { withFileTypes: true }); } catch { return []; } };
  for (const sub of ['src/bin', 'tests', 'examples', 'benches']) {
    for (const e of list(sub)) {
      if (e.isFile() && e.name.endsWith('.rs')) out.push(rel(sub, e.name));
      else if (e.isDirectory()) out.push(rel(sub, e.name, 'main.rs'));
    }
  }
  return out.filter((f) => fs.existsSync(path.join(proj.root, f)));
}

/**
 * Lints forbidden per file: the manifest forbids of the crate, plus `#![forbid(..)]`
 * inner attributes of each crate root and module, inherited down the module
 * tree (`mod x;` -> x.rs or x/mod.rs). A file no root reaches gets the manifest
 * forbids and its own inner forbids.
 * @returns {(rel: string) => Set<string>}
 */
function forbidsByFile(proj, crate, roots, manifestForbids) {
  const byFile = new Map();
  const code = new Map();
  const masked = (rel) => {
    if (!code.has(rel)) { const src = readText(path.join(proj.root, rel)); code.set(rel, src === null ? null : maskRust(src).code); }
    return code.get(rel);
  };
  const visit = (rel, inherited, isRoot, depth) => {
    const c = masked(rel);
    if (c === null || depth > 64) return;
    const set = new Set([...inherited, ...innerForbids(c)]);
    const prev = byFile.get(rel);
    if (prev && [...set].every((x) => prev.has(x))) return;
    byFile.set(rel, prev ? new Set([...prev, ...set]) : set);
    const base = path.posix.basename(rel);
    const dir = isRoot || base === 'mod.rs' ? path.posix.dirname(rel) : path.posix.join(path.posix.dirname(rel), base.replace(/\.rs$/, ''));
    let m;
    const re = new RegExp(MOD_DECL.source, 'g');
    while ((m = re.exec(c))) {
      for (const cand of [path.posix.join(dir, `${m[1]}.rs`), path.posix.join(dir, m[1], 'mod.rs')]) {
        if (fs.existsSync(path.join(proj.root, cand))) { visit(cand, set, false, depth + 1); break; }
      }
    }
  };
  for (const r of roots ?? conventionalRoots(proj, crate.dir)) visit(r, manifestForbids, true, 0);
  return (rel) => byFile.get(rel) ?? new Set([...manifestForbids, ...innerForbids(masked(rel) ?? '')]);
}

/**
 * Is a project-relative file part of its member crate? Not when a directory
 * between the member and the file has its own Cargo.toml (a nested package that
 * is not a member: vendor/, fuzz/, xtask/), or when the workspace excludes it.
 */
function inMember(rel, krate, { memberDirs, exclude, hasManifest }) {
  if (exclude.some((x) => rel === x || rel.startsWith(`${x}/`))) return false;
  for (let d = path.posix.dirname(rel); d !== '.' && d !== '' && d !== krate.dir; d = path.posix.dirname(d)) {
    if (!memberDirs.has(d) && hasManifest(d)) return false;
  }
  return true;
}

/**
 * Scanner findings (hard and ask hits) in the .rs files of workspace members.
 * Ask hits (approved `#[expect]` exceptions) carry `exception`. `only` limits
 * the rules (the forbid-override check alone when the scan source is off).
 * `roots` (crate name -> target root files, from targetRoots) drives the
 * `#![forbid]` inheritance; without it the conventional roots are used.
 */
export function scanFindings(proj, crates, { cfg = proj.config, map = ruleMap(), workspaceRoot = proj.root, only = null, roots = null } = {}) {
  const out = [];
  const exempt = cfg?.scanner?.exempt ?? [];
  const root = parseToml(readText(path.join(workspaceRoot, 'Cargo.toml')));
  const wsRel = relTo(proj.root, workspaceRoot);
  const exclude = (root?.workspace?.exclude ?? []).map((x) => path.posix.normalize(`${wsRel ? `${wsRel}/` : ''}${norm(String(x))}`).replace(/\/+$/, ''));
  const manifests = new Map();
  const hasManifest = (d) => {
    if (!manifests.has(d)) manifests.set(d, fs.existsSync(path.join(proj.root, d, 'Cargo.toml')));
    return manifests.get(d);
  };
  const scope = { memberDirs: new Set(crates.map((c) => c.dir)), exclude, hasManifest };
  const manifestForbids = forbiddenLints(proj, crates, workspaceRoot);
  const forbidOf = new Map();
  const forbidsFor = (krate, rel) => {
    if (!forbidOf.has(krate.name)) forbidOf.set(krate.name, forbidsByFile(proj, krate, roots?.get(krate.name), manifestForbids.get(krate.name) ?? new Set()));
    return forbidOf.get(krate.name)(rel);
  };
  for (const abs of rustFiles(proj)) {
    const rel = relTo(proj.root, abs);
    const krate = crateFor(rel, crates);
    if (!krate || !inMember(rel, krate, scope)) continue; // not compiled as part of the workspace
    const src = readText(abs);
    if (src === null) continue;
    const crateRel = krate.dir === '.' ? rel : rel.slice(krate.dir.length + 1);
    const hasLib = fs.existsSync(path.join(proj.root, krate.dir, 'src', 'lib.rs'));
    const kind = classifyPath(crateRel, hasLib);
    for (const h of scanSource(src, { kind, build: isBuildScript(crateRel), forbid: forbidsFor(krate, rel) }).hits) {
      if (h.severity !== 'hard' && h.severity !== 'ask') continue;
      if (only && !only.includes(h.key)) continue;
      if (exempt.some((e) => e.rule === h.id && (e.paths ?? []).some((p) => globMatch(p, rel)))) continue;
      const rules = map.scan[h.key] ?? [h.id];
      out.push({
        tool: 'scan', lint: `scan:${h.key}`, crate: krate.name, file: rel, line: h.line, end_line: h.line, message: h.msg,
        rules: [...rules], severity: severityOf(rules, map),
        ...(h.severity === 'ask' ? { exception: h.exception ?? h.key } : {}),
      });
    }
  }
  return out;
}

/** The toolchain release and host triple (`rustc -vV`), honouring rust-toolchain.toml. */
function toolchainOf(proj) {
  const r = run('rustc', ['-vV'], { cwd: proj.root, timeoutMs: 60000 });
  return { toolchain: /^release:\s*(\S+)/m.exec(r.stdout)?.[1] ?? null, host: /^host:\s*(\S+)/m.exec(r.stdout)?.[1] ?? null };
}

function cargoFailure(tool, r, cmd) {
  if (r.error) return { tool, code: null, file: null, line: null, message: `could not run \`${cmd.join(' ')}\`: ${r.error.message}` };
  const why = r.stderr.split(/\r?\n/).filter((l) => /^\s*(error|warning: build failed)/.test(l) || /error:/.test(l)).slice(0, 8).join('\n')
    || r.stderr.trim().split(/\r?\n/).slice(-8).join('\n');
  return { tool, code: null, file: null, line: null, message: `\`${cmd.slice(0, 3).join(' ')}\` failed (exit ${r.code}):\n${why}` };
}

/**
 * Run cargo with stdout going to a file, never through a pipe buffer: a large
 * brownfield workspace prints far more JSON than any maxBuffer allows.
 */
function runToFile(cmd, { cwd, env, timeoutMs }) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'temper-cargo-'));
  const file = path.join(dir, 'stdout.jsonl');
  const fd = fs.openSync(file, 'w');
  let r;
  try {
    r = spawnSync(cmd[0], cmd.slice(1), {
      cwd, env: { ...process.env, ...env }, encoding: 'utf8', stdio: ['ignore', fd, 'pipe'],
      maxBuffer: 64 * 1024 * 1024, timeout: timeoutMs, windowsHide: true,
    });
  } finally { fs.closeSync(fd); }
  return { code: r.status ?? (r.error ? 127 : 1), stderr: r.stderr ?? '', error: r.error, file, cleanup: () => fs.rmSync(dir, { recursive: true, force: true }) };
}

/** Run one cargo source and stream-parse its output; the human-readable diagnostics go to the log. */
function cargoSource(proj, tool, { cfg, workspaceRoot, logPrefix, map, locked }) {
  // --locked (CI): fail rather than re-resolve when Cargo.lock is stale (PROC-13).
  const feat = [...(cfg.gate?.allFeatures ? ['--all-features'] : []), ...(locked ? ['--locked'] : [])];
  const env = { CARGO_TERM_COLOR: 'never', CARGO_BUILD_WARNINGS: 'warn' };
  // json-diagnostic-short keeps every span but renders each diagnostic on one line (smaller output and log).
  let cmd;
  if (tool === 'clippy') {
    cmd = ['cargo', 'clippy', '--workspace', '--all-targets', ...feat, '--message-format=json-diagnostic-short', '--', '--cap-lints', 'warn'];
  } else {
    cmd = ['cargo', 'doc', '--workspace', '--no-deps', ...feat, '--message-format=json-diagnostic-short'];
    env.RUSTDOCFLAGS = '--cap-lints warn';
  }
  const logFile = logPrefix ? `${logPrefix}-${tool}.log` : null;
  let logFd = null;
  try { if (logFile) { logFd = fs.openSync(logFile, 'w'); fs.writeSync(logFd, `$ ${cmd.join(' ')}\n`); } } catch { logFd = null; }
  const r = runToFile(cmd, { cwd: proj.root, env, timeoutMs: (cfg.gate?.stepTimeoutSec ?? 900) * 1000 });
  const parser = messageParser({ tool, workspaceRoot, projectRoot: proj.root, map, onRendered: logFd === null ? null : (s) => fs.writeSync(logFd, s) });
  try { forEachLine(r.file, (l) => parser.line(l)); } catch { /* no output */ } finally { r.cleanup(); }
  const parsed = parser.result();
  if (r.code !== 0 && !parsed.errors.length) parsed.errors.push(cargoFailure(tool, r, cmd));
  if (logFd !== null) { try { fs.writeSync(logFd, `\n${r.stderr}`); fs.closeSync(logFd); } catch { /* ignore */ } }
  return { ...parsed, log: logFd === null ? null : logFile };
}

/**
 * Collect findings.
 * @param {object} proj from discover()
 * @param {{ cfg?: object, sources?: string[], logPrefix?: string, reuse?: object, locked?: boolean }} opts
 *   `sources` defaults to configuredSources(cfg): clippy, rustdoc unless gate.doc is off, scan unless gate.scan is off.
 *   `reuse`: an earlier collection of the same run; its crates and toolchain are reused (no second cargo metadata).
 * @returns {{ temper_findings: 1, at: string, commit: string|null, toolchain: string|null, host: string|null,
 *   sources: string[], workspace_root: string, crates: object[], findings: object[], errors: object[], logs?: object }}
 *   `workspace_root` is the Cargo workspace root relative to the project root ('.' when they coincide).
 */
export function collectFindings(proj, { cfg = proj.config, sources, logPrefix, map = ruleMap(), reuse, locked = false } = {}) {
  const want = sources ?? configuredSources(cfg);
  const collection = {
    temper_findings: 1, at: new Date().toISOString(), commit: headRev(proj), toolchain: null, host: null,
    sources: SOURCES.filter((s) => want.includes(s)), workspace_root: '.', crates: [], findings: [], errors: [], logs: {},
  };
  let workspaceRoot;
  let roots = null;
  if (reuse?.crates?.length && reuse.workspace_root) {
    // A later source in the same run (rustdoc after clippy): the workspace has not changed.
    Object.assign(collection, { toolchain: reuse.toolchain, host: reuse.host ?? null, crates: reuse.crates, workspace_root: reuse.workspace_root });
    workspaceRoot = path.resolve(proj.root, reuse.workspace_root);
  } else {
    if (!fs.existsSync(path.join(proj.root, 'Cargo.toml'))) {
      const below = findUp(process.cwd(), (d) => fs.existsSync(path.join(d, 'Cargo.toml')));
      collection.errors.push({
        tool: 'metadata', code: 'workspace-below-git-root', file: null, line: null,
        message: `temper needs the Cargo workspace at the git root: ${proj.root} has no Cargo.toml${below ? ` (the workspace is at ${relTo(proj.root, below)})` : ''}. Nested workspaces are not supported yet.`,
      });
      return collection;
    }
    const meta = run('cargo', ['metadata', '--no-deps', '--format-version', '1', ...(locked ? ['--locked'] : [])], { cwd: proj.root, env: { CARGO_TERM_COLOR: 'never' }, timeoutMs: 120000 });
    let parsed = null;
    try { parsed = meta.code === 0 ? JSON.parse(meta.stdout) : null; } catch { parsed = null; }
    if (!parsed) {
      collection.errors.push(cargoFailure('metadata', meta, ['cargo', 'metadata', '--no-deps']));
      return collection;
    }
    Object.assign(collection, toolchainOf(proj));
    collection.crates = crateTable(parsed, proj.root);
    roots = targetRoots(parsed, proj.root);
    workspaceRoot = parsed.workspace_root ?? proj.root;
    collection.workspace_root = relTo(proj.root, workspaceRoot) || '.';
  }
  const found = [];
  for (const tool of ['clippy', 'rustdoc']) {
    if (!collection.sources.includes(tool)) continue;
    const res = cargoSource(proj, tool, { cfg, workspaceRoot, logPrefix, map, locked });
    found.push(...res.findings);
    collection.errors.push(...res.errors);
    if (res.log) collection.logs[tool] = res.log;
  }
  if (collection.sources.includes('scan')) found.push(...scanFindings(proj, collection.crates, { cfg, map, workspaceRoot, roots }));
  // The E0453 stand-in belongs to the build, so it is checked even with the scan source off.
  else if (collection.sources.includes('clippy')) found.push(...scanFindings(proj, collection.crates, { cfg, map, workspaceRoot, roots, only: ['forbid-override'] }));
  collection.findings = sortFindings(dedupeFindings(found.map((f) => ({ ...f, crate: f.crate ?? crateFor(f.file, collection.crates)?.name ?? null }))));
  if (!Object.keys(collection.logs).length) delete collection.logs;
  return collection;
}

/** Merge a later partial collection (say, rustdoc after clippy) into an earlier one. */
export function mergeCollections(a, b) {
  const sources = SOURCES.filter((s) => a.sources.includes(s) || b.sources.includes(s));
  return {
    ...a,
    toolchain: a.toolchain ?? b.toolchain,
    host: a.host ?? b.host ?? null,
    sources,
    crates: a.crates.length ? a.crates : b.crates,
    findings: sortFindings(dedupeFindings([...a.findings, ...b.findings])),
    errors: [...a.errors, ...b.errors],
    ...(a.logs || b.logs ? { logs: { ...(a.logs ?? {}), ...(b.logs ?? {}) } } : {}),
  };
}

/** The sources a configuration collects: clippy always, rustdoc unless `gate.doc` is off, scan unless `gate.scan` is off. */
export function configuredSources(cfg) {
  return SOURCES.filter((s) => (s !== 'rustdoc' || cfg?.gate?.doc !== false) && (s !== 'scan' || cfg?.gate?.scan !== false));
}

/** Is `collection` the full set of sources this configuration collects? */
export function isComplete(collection, cfg) {
  return configuredSources(cfg).every((s) => collection?.sources?.includes(s));
}

// ------------------------------------------------------------------ cache

export const findingsCachePath = (proj, fp) => path.join(proj.stateDir, `findings-${fp}.json`);

/**
 * The collection cached by the gate for this fingerprint, if it was taken at
 * the current HEAD with the same base (a fingerprint is a diff against the base).
 */
export function cachedFindings(proj, fp, { base } = {}) {
  const t = readText(findingsCachePath(proj, fp));
  if (!t) return null;
  try {
    const c = JSON.parse(t);
    if (c.temper_findings !== 1) return null;
    if ((c.cache?.commit ?? null) !== headRev(proj)) return null;
    if (base !== undefined && (c.cache?.base ?? null) !== (base ?? null)) return null;
    return c;
  } catch { return null; }
}

const CACHE_KEEP = 20;

/** Cache a complete collection for this fingerprint; only the newest CACHE_KEEP collections are kept. */
export function writeCachedFindings(proj, fp, collection, { base } = {}) {
  if (!fp) return;
  try { writeAtomic(findingsCachePath(proj, fp), JSON.stringify({ ...collection, cache: { fp, base: base ?? null, commit: headRev(proj) } }, null, 2)); } catch { return; }
  pruneFindingsCache(proj);
}

export function pruneFindingsCache(proj, keep = CACHE_KEEP) {
  let files = [];
  try { files = fs.readdirSync(proj.stateDir).filter((f) => /^findings-[0-9a-f]+\.json$/.test(f)); } catch { return; }
  if (files.length <= keep) return;
  const aged = files.map((f) => { const p = path.join(proj.stateDir, f); try { return { p, t: fs.statSync(p).mtimeMs }; } catch { return null; } }).filter(Boolean);
  aged.sort((a, b) => b.t - a.t);
  for (const { p } of aged.slice(keep)) { try { fs.rmSync(p, { force: true }); } catch { /* ignore */ } }
}
