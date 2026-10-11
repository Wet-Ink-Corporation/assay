// Project discovery, state storage, configuration and tree fingerprinting.
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

export const norm = (p) => p.replace(/\\/g, '/');

export function run(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, {
    // stdin is /dev/null: cargo's rustc probes read stdin, and a hook's stdin is not theirs.
    cwd: opts.cwd, env: { ...process.env, ...(opts.env ?? {}) }, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
    maxBuffer: 256 * 1024 * 1024, timeout: opts.timeoutMs ?? 0, windowsHide: true,
  });
  return { code: r.status ?? (r.error ? 127 : 1), stdout: r.stdout ?? '', stderr: r.stderr ?? '', error: r.error };
}

const git = (cwd, args, opts) => run('git', args, { cwd, ...opts });

export function readText(p) {
  try { return fs.readFileSync(p, 'utf8'); } catch { return null; }
}

export function writeAtomic(p, text) {
  fs.mkdirSync(path.dirname(p), { recursive: true });
  const tmp = `${p}.${process.pid}.${Date.now()}.tmp`;
  fs.writeFileSync(tmp, text);
  fs.renameSync(tmp, p);
}

/** Nearest ancestor (inclusive) containing `name`. */
export function findUp(start, predicate) {
  let dir = path.resolve(start);
  for (;;) {
    if (predicate(dir)) return dir;
    const parent = path.dirname(dir);
    if (parent === dir) return null;
    dir = parent;
  }
}

const hasFile = (name) => (dir) => fs.existsSync(path.join(dir, name));

/**
 * Locate the project: git top-level if inside a repo, otherwise the outermost
 * Cargo workspace root above `cwd`. Returns null when this isn't a Rust project.
 */
export function discover(cwd) {
  const start = cwd && fs.existsSync(cwd) ? cwd : process.cwd();
  const top = git(start, ['rev-parse', '--show-toplevel']);
  const gitRoot = top.code === 0 ? path.resolve(top.stdout.trim()) : null;
  let root = gitRoot;
  if (!root) {
    let dir = findUp(start, hasFile('Cargo.toml'));
    let outer = dir;
    while (dir) {
      outer = dir;
      const parent = path.dirname(dir);
      dir = parent === dir ? null : findUp(parent, hasFile('Cargo.toml'));
    }
    root = outer;
  }
  if (!root) return null;
  const isRust = fs.existsSync(path.join(root, 'Cargo.toml')) || fs.existsSync(path.join(start, 'Cargo.toml'))
    || findUp(start, hasFile('Cargo.toml')) !== null;
  if (!isRust) return null;
  let stateDir;
  if (gitRoot) {
    const common = git(gitRoot, ['rev-parse', '--git-common-dir']);
    const gd = common.code === 0 ? path.resolve(gitRoot, common.stdout.trim()) : path.join(gitRoot, '.git');
    stateDir = path.join(gd, 'temper');
  } else {
    stateDir = path.join(root, 'target', 'temper');
  }
  return { root, gitRoot, stateDir, config: loadConfig(root) };
}

export const DEFAULT_CONFIG = {
  skillGate: true,
  review: 'required',              // required | advisory | off
  askOnNewDependency: true,
  // Dependencies the standard itself relies on; adding them does not prompt (H-16 pre-approval).
  preapprovedDependencies: ['thiserror', 'anyhow', 'serde', 'serde_json', 'tracing', 'tracing-subscriber', 'proptest', 'insta', 'trybuild', 'divan', 'criterion', 'tempfile'],
  gate: {
    allFeatures: true,
    doc: true,
    scan: true,
    stepTimeoutSec: 900,
    steps: null,                   // override: [{ name, cmd: [..], env: {..} }]
  },
  protected: [
    '**/clippy.toml', '**/.clippy.toml', '**/rustfmt.toml', '**/.rustfmt.toml', '**/deny.toml',
    'rust-toolchain', 'rust-toolchain.toml', '.cargo/**', '.claude/**', '.github/**',
    '.githooks/**', 'CLAUDE.md', 'AGENTS.md',
  ],
  scanner: { exempt: [] },         // [{ rule: 'H-01', paths: ['crates/ffi-sys/**'] }]
  // Members (dirs relative to the project root, globs allowed) that a human approved to carry
  // their own full [lints] table instead of `[lints] workspace = true`: the H-01 FFI exception,
  // the ERR-09 arithmetic opt-in. Any change to their [lints] still asks (H-05).
  manifests: { ownLints: [] },
  // Opt-in: the branch on origin that Stop pushes each session's telemetry to (pushTelemetry).
  telemetry: { branch: null },
};

export function loadConfig(root) {
  const text = readText(path.join(root, '.claude', 'temper.json'));
  if (!text) return structuredClone(DEFAULT_CONFIG);
  try {
    const user = JSON.parse(text);
    return {
      ...DEFAULT_CONFIG, ...user,
      gate: { ...DEFAULT_CONFIG.gate, ...(user.gate ?? {}) },
      scanner: { ...DEFAULT_CONFIG.scanner, ...(user.scanner ?? {}) },
      manifests: { ...DEFAULT_CONFIG.manifests, ...(user.manifests ?? {}) },
      telemetry: { ...DEFAULT_CONFIG.telemetry, ...(user.telemetry ?? {}) },
      protected: user.protected ?? DEFAULT_CONFIG.protected,
    };
  } catch {
    return structuredClone(DEFAULT_CONFIG);
  }
}

// Glob match: "**/" = any directories, "**" = anything, "*" = within a segment, "?" = one char.
export function globMatch(pattern, rel) {
  let re = '';
  const p = norm(pattern);
  for (let i = 0; i < p.length; i++) {
    const c = p[i];
    if (c === '*' && p[i + 1] === '*') {
      if (p[i + 2] === '/') { re += '(?:.*/)?'; i += 2; } else { re += '.*'; i += 1; }
    } else if (c === '*') re += '[^/]*';
    else if (c === '?') re += '[^/]';
    else re += c.replace(/[.+^${}()|[\]\\]/g, '\\$&');
  }
  return new RegExp(`^${re}$`).test(norm(rel));
}

// cargo and rustfmt on Windows can report verbatim paths (\\?\C:\..., //?/C:/...), which
// path.relative treats as a different root from C:\...
const unverbatim = (p) => (process.platform === 'win32'
  ? p.replace(/^[\\/]{2}\?[\\/](UNC[\\/])?/i, (_m, unc) => (unc ? '\\\\' : ''))
  : p);

// The long, real form of a path that may not exist yet: its nearest existing ancestor
// through realpath, plus the rest.
function longPath(p) {
  try {
    return fs.realpathSync.native(p);
  } catch {
    const parent = path.dirname(p);
    return parent === p ? p : path.join(longPath(parent), path.basename(p));
  }
}

export function relTo(root, file) {
  const r = unverbatim(root);
  const f = path.resolve(unverbatim(file));
  const rel = path.relative(r, f);
  // One directory can be spelled two ways: through a symlinked ancestor (macOS TMPDIR under
  // /var, a link to the /private/var that cargo reports) or, on Windows, with 8.3 short names
  // (C:\Users\RUNNER~1) from TEMP and the cwd against long names from git. Compare the real,
  // long forms when the lexical ones put the file outside the root.
  if (rel.startsWith('..') || path.isAbsolute(rel)) {
    return norm(path.relative(longPath(r), longPath(f)));
  }
  return norm(rel);
}

/** Crate root (dir with a Cargo.toml containing [package]) for a file. */
export function crateOf(file, stopAt) {
  const dir = findUp(path.dirname(path.resolve(file)), (d) => {
    const t = readText(path.join(d, 'Cargo.toml'));
    return t !== null && /^\s*\[package\]/m.test(t);
  });
  if (!dir) return null;
  if (stopAt && !path.resolve(dir).startsWith(path.resolve(stopAt))) return null;
  return { dir, hasLib: fs.existsSync(path.join(dir, 'src', 'lib.rs')) };
}

// ---------- session state ----------

export function sessionDir(proj, sid) {
  return path.join(proj.stateDir, 'sessions', String(sid || 'unknown').replace(/[^\w.-]/g, '_'));
}

export function agentKey(input) {
  return input.agent_id ? `agent-${String(input.agent_id).replace(/[^\w.-]/g, '_')}` : 'main';
}

// Skill-load markers are per session and agent context, independent of which
// project the session happens to be in (the session may start outside the repo).
function markerDir(input) {
  const base = process.env.CLAUDE_PLUGIN_DATA || path.join(os.tmpdir(), 'temper-plugin-data');
  return path.join(base, 'sessions', String(input.session_id || 'unknown').replace(/[^\w.-]/g, '_'));
}

export function markSkillLoaded(_proj, input) {
  writeAtomic(path.join(markerDir(input), `skill-${agentKey(input)}`), new Date().toISOString());
}

export function skillLoaded(_proj, input) {
  return fs.existsSync(path.join(markerDir(input), `skill-${agentKey(input)}`));
}

export function pruneMarkers(maxAgeDays = 7) {
  const dir = path.join(process.env.CLAUDE_PLUGIN_DATA || path.join(os.tmpdir(), 'temper-plugin-data'), 'sessions');
  let entries = [];
  try { entries = fs.readdirSync(dir); } catch { return; }
  const cutoff = Date.now() - maxAgeDays * 86400000;
  for (const e of entries) {
    const p = path.join(dir, e);
    try { if (fs.statSync(p).mtimeMs < cutoff) fs.rmSync(p, { recursive: true, force: true }); } catch { /* ignore */ }
  }
}

/**
 * Where temper may write an output (`baseline --findings-out`, `report --out`):
 * under the project's target/ or .temper/ after every symlink is resolved, with
 * no `..`, and never onto a symlink. So an allow-listed command cannot be aimed
 * at sources or configuration, not even through a symlinked directory.
 * @param {string} root the project root
 * @param {string} out the path as given (relative to the cwd)
 * @param {{ dir?: boolean, files?: string[] }} opts `dir`: `out` is a directory
 *   to write into (it may be target/ or .temper/ itself); `files`: the names
 *   that will be written inside it
 * @returns {string|null} the absolute path, or null when refused
 */
export function safeOutputPath(root, out, { dir = false, files = [] } = {}) {
  if (!out || norm(String(out)).split('/').includes('..')) return null;
  const abs = path.resolve(out);
  const rel = relTo(root, abs);
  if (!(dir ? /^(?:target|\.temper)(?:\/.|$)/ : /^(?:target|\.temper)\/./).test(rel)) return null;
  const real = (p) => { try { return fs.realpathSync.native(p); } catch { return null; } };
  const realRoot = real(root) ?? path.resolve(root);
  const allowed = ['target', '.temper'].map((d) => {
    const p = path.join(root, d);
    try { fs.lstatSync(p); } catch { return path.join(realRoot, d); } // not there yet: it will be created under the root
    return real(p); // a dangling link resolves to null and allows nothing
  }).filter(Boolean);
  // The deepest existing ancestor of the directory that will hold the output, resolved.
  let existing = dir ? abs : path.dirname(abs);
  const rest = [];
  for (;;) {
    try { fs.lstatSync(existing); break; } catch { /* not there yet */ }
    const up = path.dirname(existing);
    if (up === existing) return null;
    rest.unshift(path.basename(existing));
    existing = up;
  }
  const resolvedBase = real(existing);
  if (!resolvedBase) return null; // a dangling symlink on the way
  const resolved = path.join(resolvedBase, ...rest);
  if (!allowed.some((a) => resolved === a || resolved.startsWith(`${a}${path.sep}`))) return null;
  const finals = dir ? files.map((f) => path.join(abs, f)) : [abs];
  for (const f of finals) { try { if (fs.lstatSync(f).isSymbolicLink()) return null; } catch { /* not there yet */ } }
  return abs;
}

/** A revision as a commit sha of this repository, or null when it does not resolve. */
export function resolveRev(proj, rev) {
  if (!proj?.gitRoot || !rev) return null;
  const r = git(proj.gitRoot, ['rev-parse', '--verify', '--quiet', `${rev}^{commit}`]);
  return r.code === 0 ? r.stdout.trim() || null : null;
}

export function headRev(proj) {
  if (!proj.gitRoot) return null;
  const r = git(proj.gitRoot, ['rev-parse', 'HEAD']);
  return r.code === 0 ? r.stdout.trim() : null;
}

export function baseRev(proj, sid) {
  const t = readText(path.join(sessionDir(proj, sid), 'base'));
  // A recorded base that no longer resolves (history rewritten, another repository): HEAD.
  return (t && t.trim() && resolveRev(proj, t.trim())) || headRev(proj);
}

export function setBase(proj, sid, force) {
  const p = path.join(sessionDir(proj, sid), 'base');
  if (!force && fs.existsSync(p)) return;
  const head = headRev(proj);
  writeAtomic(p, head ?? '');
  writeAtomic(path.join(sessionDir(proj, sid), 'started'), String(Date.now()));
  // Fingerprint of the tree as the session found it: uncommitted work that
  // predates the session (e.g. a diff the user asked us to review) is not ours.
  writeAtomic(path.join(sessionDir(proj, sid), 'start-fp'), fingerprint(proj, head));
}

export function startFingerprint(proj, sid) {
  const t = readText(path.join(sessionDir(proj, sid), 'start-fp'));
  return t ? t.trim() : null;
}

export function pruneSessions(proj, maxAgeDays = 14) {
  const dir = path.join(proj.stateDir, 'sessions');
  let entries = [];
  try { entries = fs.readdirSync(dir); } catch { return; }
  const cutoff = Date.now() - maxAgeDays * 86400000;
  for (const e of entries) {
    const p = path.join(dir, e);
    try { if (fs.statSync(p).mtimeMs < cutoff) fs.rmSync(p, { recursive: true, force: true }); } catch { /* ignore */ }
  }
}

/**
 * How long the per-fingerprint records under the state dir are kept: by age
 * (newest mtime) and by count, newest first, and for gate runs also by total
 * size. A gate run is gate/<fp>.json together with its logs/<fp>-* files, and
 * they are pruned as one, so a kept result never cites a deleted log.
 * SessionStart prunes them on startup (pruneFingerprintState). A verdict is
 * the costliest to lose (a fresh reviewer run); a gate run costs a gate re-run.
 * telemetry.jsonl rotates to telemetry.1.jsonl at telemetryBytes, and one old
 * file is kept. A record touched within graceMinutes is never pruned: the state
 * dir is shared by every worktree, and another session's gate may be writing
 * the logs of a fingerprint that is not this session's.
 */
export const RETENTION = {
  verdicts: { maxAgeDays: 60, keep: 300 },
  gate: { maxAgeDays: 30, keep: 300, maxBytes: 64 * 1024 * 1024 },
  telemetryBytes: 1024 * 1024,
  graceMinutes: 60,
};

/**
 * Group the files of `dirs` by fingerprint: the name up to its first `-` or `.`.
 * Each group has its files, its newest mtime and its total size.
 */
function fingerprintGroups(dirs) {
  const groups = new Map();
  for (const dir of dirs) {
    let names = [];
    try { names = fs.readdirSync(dir); } catch { continue; }
    for (const n of names) {
      const p = path.join(dir, n);
      let s;
      try { s = fs.statSync(p); } catch { continue; }
      if (!s.isFile()) continue;
      const key = n.split(/[-.]/)[0];
      const g = groups.get(key) ?? { key, files: [], t: 0, size: 0 };
      g.files.push(p);
      g.t = Math.max(g.t, s.mtimeMs);
      g.size += s.size;
      groups.set(key, g);
    }
  }
  return [...groups.values()];
}

/**
 * Remove the groups of `dirs` older than `maxAgeDays`, then all but the newest
 * `keep`, then the oldest until the rest fit in `maxBytes` (the newest group
 * always stays). The group `protect` names is never removed and counts toward
 * no limit. A group whose newest file is younger than RETENTION.graceMinutes is
 * never removed either, though it counts toward the limits. Errors (a file in
 * use on Windows, a race with another session) are ignored.
 */
function pruneGroups(dirs, { maxAgeDays, keep, maxBytes = Infinity }, protect) {
  const now = Date.now();
  const cutoff = now - maxAgeDays * 86400000;
  const recent = now - RETENTION.graceMinutes * 60000;
  const groups = fingerprintGroups(dirs).filter((g) => !protect || g.key !== protect).sort((a, b) => b.t - a.t);
  let bytes = 0;
  groups.forEach((g, i) => {
    bytes += g.size;
    if (g.t < recent && (g.t < cutoff || i >= keep || (i > 0 && bytes > maxBytes))) {
      for (const p of g.files) { try { fs.rmSync(p, { force: true }); } catch { /* ignore */ } }
    }
  });
}

/**
 * Prune verdicts/<fp>.json, and gate/<fp>.json with its logs/<fp>-* files, to
 * the RETENTION limits. The records of `currentFp`, the tree the session
 * starts on, are kept whatever their age. A pruned verdict means a later Stop
 * on that tree asks for a fresh review; a pruned gate result, a fresh gate run.
 */
export function pruneFingerprintState(proj, currentFp) {
  pruneGroups([path.join(proj.stateDir, 'verdicts')], RETENTION.verdicts, currentFp);
  pruneGroups([path.join(proj.stateDir, 'gate'), path.join(proj.stateDir, 'logs')], RETENTION.gate, currentFp);
}

const TELEMETRY = 'telemetry.jsonl';
const TELEMETRY_OLD = 'telemetry.1.jsonl';

export function telemetry(proj, input, event, detail = {}) {
  try {
    fs.mkdirSync(proj.stateDir, { recursive: true });
    const file = path.join(proj.stateDir, TELEMETRY);
    // Rotate at the cap and keep one old file, so the pair stays under about twice the cap.
    // Two hooks rotating at once can drop the older file's history; telemetry is best effort.
    try { if (fs.statSync(file).size >= RETENTION.telemetryBytes) fs.renameSync(file, path.join(proj.stateDir, TELEMETRY_OLD)); } catch { /* absent, or rotated by another hook */ }
    // ccr: the Claude Code cloud session (cse_X is claude.ai/code/session_X), which links the row to the
    // Claude-Session trailer of the commits it made; sid is the CLI's own session id.
    const ccr = process.env.CLAUDE_CODE_REMOTE_SESSION_ID || undefined;
    fs.appendFileSync(file, JSON.stringify({
      t: new Date().toISOString(), sid: input?.session_id, ...(ccr ? { ccr } : {}), agent: input?.agent_type ?? 'main', event, ...detail,
    }) + '\n');
  } catch { /* best effort */ }
}

const parseRows = (text) => String(text ?? '').split('\n')
  .filter(Boolean).map((l) => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);

/** The telemetry rows that remain, oldest file first; lines that do not parse are skipped. */
export function readTelemetry(proj) {
  return [TELEMETRY_OLD, TELEMETRY].flatMap((f) => parseRows(readText(path.join(proj.stateDir, f))));
}

const TELEMETRY_TIP = 'refs/temper/telemetry-tip';

/**
 * Keep a session's telemetry beyond its machine. A Claude Code cloud session runs in a container
 * that is discarded when the session ends, and .git/temper/ with it, so its telemetry never
 * reaches `temper stats`. With `telemetry.branch` set in .claude/temper.json, Stop runs this (in a
 * detached process) to push the session's rows to that branch on `remote` as
 * sessions/<date>-<session>.jsonl, rewritten whole on each push. It is temper's only network call,
 * and it is off by default. It builds the commit with a private index and a private ref, so the
 * working tree, the index, HEAD and the clone's branches are untouched, and the commit message
 * carries [skip ci] so the push starts no workflow. A rejected push (another session pushed first)
 * is retried on the new tip: each session writes only its own file.
 */
export function pushTelemetry(proj, sid, { remote = 'origin', attempts = 3 } = {}) {
  const branch = proj.config.telemetry?.branch;
  if (!branch || !proj.gitRoot) return { pushed: false, why: 'telemetry.branch is not set' };
  const rows = readTelemetry(proj).filter((r) => r.sid === sid);
  if (!rows.length) return { pushed: false, why: 'no telemetry for this session' };
  const text = rows.map((r) => JSON.stringify(r)).join('\n') + '\n';
  const digest = createHash('sha256').update(text).digest('hex');
  const mark = path.join(sessionDir(proj, sid), 'telemetry-pushed');
  if (readText(mark) === digest) return { pushed: false, why: 'unchanged since the last push' };
  const key = String(rows.find((r) => r.ccr)?.ccr ?? sid).replace(/[^\w.-]/g, '_');
  const file = `sessions/${String(rows[0].t ?? '').slice(0, 10)}-${key}.jsonl`;
  const who = { name: 'temper', email: 'temper@users.noreply.github.com' };
  const env = {
    GIT_INDEX_FILE: path.join(proj.stateDir, `telemetry-index.${process.pid}`),
    GIT_AUTHOR_NAME: who.name, GIT_AUTHOR_EMAIL: who.email, GIT_COMMITTER_NAME: who.name, GIT_COMMITTER_EMAIL: who.email,
  };
  const g = (args) => git(proj.gitRoot, args, { env, timeoutMs: 60000 });
  const tmp = path.join(proj.stateDir, `telemetry-push.${process.pid}.jsonl`);
  let why = 'git push failed';
  try {
    fs.writeFileSync(tmp, text);
    const blob = g(['hash-object', '-w', tmp]).stdout.trim();
    for (let i = 0; i < attempts; i += 1) {
      // A branch that does not exist yet fails the fetch; the push then creates it from a root commit.
      const fetched = g(['fetch', '--quiet', '--no-tags', remote, `+refs/heads/${branch}:${TELEMETRY_TIP}`]).code === 0;
      const tip = fetched ? g(['rev-parse', '--verify', '--quiet', TELEMETRY_TIP]).stdout.trim() : '';
      g(tip ? ['read-tree', tip] : ['read-tree', '--empty']);
      g(['update-index', '--add', '--cacheinfo', `100644,${blob},${file}`]);
      const tree = g(['write-tree']).stdout.trim();
      const commit = tree ? g(['commit-tree', tree, ...(tip ? ['-p', tip] : []), '-m', `telemetry: ${key} [skip ci]`]).stdout.trim() : '';
      if (!commit) { why = 'git could not build the telemetry commit'; break; }
      const pushed = g(['push', '--quiet', remote, `${commit}:refs/heads/${branch}`]);
      if (pushed.code === 0) { writeAtomic(mark, digest); return { pushed: true, branch, file, rows: rows.length }; }
      why = pushed.stderr.trim().split('\n').at(-1) || why;
    }
  } catch (e) {
    why = e?.message ?? String(e);
  } finally {
    for (const f of [tmp, env.GIT_INDEX_FILE]) { try { fs.rmSync(f, { force: true }); } catch { /* ignore */ } }
  }
  return { pushed: false, why };
}

/** Every session's rows on `branch` of `remote`, fetched fresh; null when the branch cannot be fetched. */
export function fetchTelemetryBranch(proj, branch, { remote = 'origin' } = {}) {
  if (!proj.gitRoot) return null;
  const g = (args) => git(proj.gitRoot, args, { timeoutMs: 120000 });
  if (g(['fetch', '--quiet', '--no-tags', remote, `+refs/heads/${branch}:${TELEMETRY_TIP}`]).code !== 0) return null;
  const files = g(['ls-tree', '-r', '--name-only', TELEMETRY_TIP, 'sessions/']).stdout.split('\n').filter((f) => f.endsWith('.jsonl'));
  return files.flatMap((f) => parseRows(g(['show', `${TELEMETRY_TIP}:${f}`]).stdout));
}

// ---------- change detection and fingerprint ----------

// What counts as "the Rust change": sources, manifests, toolchain and lint config,
// cargo config (rustflags), test expectations (snapshots, trybuild .stderr) and
// anything under a tests/ dir. Other files (notes, logs, transcripts) never
// invalidate a gate result or a review. Cargo reads .cargo/config(.toml) from its
// cwd and every parent, so one in any directory counts: the gate runs cargo at
// the root, but a workspace in a subdirectory, or cargo run in a member, reads a
// nested one too. temper's own config counts too (see gateConfig below).
const RUST_RELEVANT = /(\.rs$)|(\.snap$)|(\.stderr$)|((^|\/)tests\/)|((^|\/)Cargo\.(toml|lock)$)|((^|\/)(rust-toolchain(\.toml)?|\.?clippy\.toml|\.?rustfmt\.toml|deny\.toml)$)|((^|\/)\.cargo\/config(\.toml)?$)/;
const RELEVANT_PATHSPECS = [
  ':(glob)**/*.rs', ':(glob)**/*.snap', ':(glob)**/*.stderr', ':(glob)**/tests/**',
  ':(glob)**/Cargo.toml', ':(glob)**/Cargo.lock', ':(glob)**/rust-toolchain', ':(glob)**/rust-toolchain.toml',
  ':(glob)**/clippy.toml', ':(glob)**/.clippy.toml', ':(glob)**/rustfmt.toml', ':(glob)**/.rustfmt.toml', ':(glob)**/deny.toml',
  ':(glob)**/.cargo/config', ':(glob)**/.cargo/config.toml',
];
const EXCLUDE = ['.temper', 'target'];

/**
 * The committed lint baseline (temper baseline). It changes the gate's verdict,
 * so it is part of the fingerprint, but it is not a Rust change on its own.
 */
export const BASELINE_FILE = '.claude/temper-baseline.json';
const FINGERPRINT_PATHSPECS = [...RELEVANT_PATHSPECS, `:(literal)${BASELINE_FILE}`];
const inFingerprint = (rel) => RUST_RELEVANT.test(rel) || rel === BASELINE_FILE;

/**
 * temper's config at the project root (the git top-level in a repository) counts
 * by what the gate reads from it, not by its bytes: its `gate`, `scanner` and
 * `manifests` keys, merged with the defaults as loadConfig does and cut to the
 * properties the defaults list (the only ones the gate reads), as canonical
 * JSON. A change to a key that only steers hooks (review, skillGate, protected,
 * the dependency prompt), a property the gate does not read (`gate.colour`), a
 * reformat or a default spelled out leaves the gate's
 * result as it was, so it is no Rust change. The file is read directly, so one
 * that git ignores still counts.
 */
const TEMPER_CONFIG_FILE = '.claude/temper.json';
const GATE_CONFIG_KEYS = ['gate', 'scanner', 'manifests'];
const canonical = (v) => (Array.isArray(v) ? v.map(canonical)
  : v && typeof v === 'object' ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, canonical(v[k])])) : v);
function gateConfig(text) {
  let user = {};
  try { user = text ? JSON.parse(text) ?? {} : {}; } catch { /* loadConfig falls back to the defaults */ }
  if (typeof user !== 'object') user = {};
  // only the properties the gate reads, which are exactly the ones DEFAULT_CONFIG lists
  const pick = (k) => {
    const merged = { ...DEFAULT_CONFIG[k], ...(user[k] ?? {}) };
    return Object.fromEntries(Object.keys(DEFAULT_CONFIG[k]).map((p) => [p, merged[p]]));
  };
  return JSON.stringify(canonical(Object.fromEntries(GATE_CONFIG_KEYS.map((k) => [k, pick(k)]))));
}
/** The gate config now, when it differs from the one at the base (the defaults outside git); else null. */
function changedGateConfig(proj, base) {
  const file = path.join(proj.root, ...TEMPER_CONFIG_FILE.split('/'));
  const now = gateConfig(readText(file));
  // the same revision fingerprint() diffs against: the base, or HEAD when it does not resolve
  const rev = proj.gitRoot ? ((base ? resolveRev(proj, base) : null) ?? resolveRev(proj, 'HEAD')) : null;
  const before = rev ? showAt(proj, rev, relTo(proj.gitRoot, file)) : null;
  return now === gateConfig(before) ? null : now;
}

function untracked(proj) {
  const r = git(proj.gitRoot, ['ls-files', '--others', '--exclude-standard', '-z']);
  if (r.code !== 0) return [];
  return r.stdout.split('\0').filter(Boolean).filter((f) => !EXCLUDE.some((x) => f === x || f.startsWith(`${x}/`)));
}

/** Files changed since the session base (committed or not), plus untracked files. */
export function changedFiles(proj, base) {
  if (!proj.gitRoot) return walkRust(proj.root).map((f) => relTo(proj.root, f));
  const args = ['diff', '--name-only', '-z'];
  if (base) args.push(base);
  args.push('--', '.', ...EXCLUDE.map((x) => `:(exclude)${x}`));
  const r = git(proj.gitRoot, args);
  const tracked = r.code === 0 ? r.stdout.split('\0').filter(Boolean) : [];
  return [...new Set([...tracked, ...untracked(proj)])];
}

export function rustChanges(proj, base) {
  const files = changedFiles(proj, base).filter((f) => RUST_RELEVANT.test(norm(f)));
  return changedGateConfig(proj, base) === null ? files : [...files, TEMPER_CONFIG_FILE];
}

function walkRust(root) {
  const out = [];
  const walk = (d) => {
    let es = [];
    try { es = fs.readdirSync(d, { withFileTypes: true }); } catch { return; }
    for (const e of es) {
      if (e.name === 'target' || e.name === '.git' || e.name === 'node_modules' || e.name === '.temper') continue;
      const p = path.join(d, e.name);
      if (e.isDirectory()) walk(p);
      else if (RUST_RELEVANT.test(norm(p))) out.push(p);
    }
  };
  walk(root);
  return out;
}

/** Stable fingerprint of the Rust-relevant working tree relative to base. */
export function fingerprint(proj, base) {
  const h = createHash('sha256');
  if (proj.gitRoot) {
    // The tree is the base commit plus the diff, so the commits are part of the key:
    // otherwise every clean tree hashes the same and a cached result from another
    // commit would be reused. HEAD always counts; a base that does not resolve here
    // (a TEMPER_BASE from another repository) is replaced by HEAD.
    const head = resolveRev(proj, 'HEAD');
    const rev = (base ? resolveRev(proj, base) : null) ?? head;
    h.update(`head:${head ?? ''}\nrev:${rev ?? ''}\n`);
    const args = ['diff', '--no-color', '--binary', '--no-ext-diff'];
    if (rev) args.push(rev);
    args.push('--', ...FINGERPRINT_PATHSPECS, ...EXCLUDE.map((x) => `:(exclude)${x}`));
    h.update(git(proj.gitRoot, args).stdout);
    for (const f of untracked(proj).filter((u) => inFingerprint(norm(u))).sort()) {
      h.update(`\n?${f}\n`);
      try { h.update(fs.readFileSync(path.join(proj.gitRoot, f))); } catch { /* vanished */ }
    }
  } else {
    const baseline = path.join(proj.root, ...BASELINE_FILE.split('/'));
    const files = walkRust(proj.root).concat(fs.existsSync(baseline) ? [baseline] : []);
    for (const f of files.sort()) {
      h.update(`\n${relTo(proj.root, f)}\n`);
      try { h.update(fs.readFileSync(f)); } catch { /* vanished */ }
    }
  }
  const config = changedGateConfig(proj, base);
  if (config !== null) h.update(`\n${TEMPER_CONFIG_FILE}\n${config}`);
  return h.digest('hex').slice(0, 16);
}

/** File content at a revision (null if absent). */
export function showAt(proj, rev, rel) {
  if (!proj.gitRoot || !rev) return null;
  const r = git(proj.gitRoot, ['show', `${rev}:${norm(rel)}`]);
  return r.code === 0 ? r.stdout : null;
}

export function diffStat(proj, base) {
  if (!proj.gitRoot) return '';
  const args = ['diff', '--stat=120', '--no-color'];
  if (base) args.push(base);
  args.push('--', '.', ...EXCLUDE.map((x) => `:(exclude)${x}`));
  const r = git(proj.gitRoot, args);
  const u = untracked(proj).filter((f) => RUST_RELEVANT.test(norm(f)));
  return (r.stdout.trim() + (u.length ? `\nuntracked: ${u.join(', ')}` : '')).trim();
}
