// Cargo manifest checks shared by the pre-edit hook and the gate.
import fs from 'node:fs';
import path from 'node:path';
import { parseToml } from './toml.mjs';
import { readText, norm, globMatch } from './project.mjs';

/** A relative path compared the way Cargo compares them: by components (`./x/` is `x`). */
export const cleanRel = (p) => path.posix.normalize(norm(String(p))).replace(/^(?:\.\/)+/, '').replace(/\/+$/, '');

export const inheritsLints = (m) => m?.lints?.workspace === true;
export const definesWorkspaceLints = (m) => m?.workspace?.lints !== undefined;
export const hasProfile = (m) => m?.profile !== undefined;

/**
 * The workspace root that governs the package manifest at `manifestPath`,
 * following Cargo's rules: the manifest itself if it has a `[workspace]` table,
 * else `package.workspace`, else the nearest ancestor with a `[workspace]`
 * table that does not exclude it. Returns null for a standalone package.
 * @returns {{ path: string, manifest: object, self: boolean } | null}
 */
export function workspaceRootFor(manifestPath, manifest) {
  const abs = path.resolve(manifestPath);
  if (manifest?.workspace) return { path: abs, manifest, self: true };
  const crateDir = path.dirname(abs);
  const explicit = manifest?.package?.workspace;
  if (typeof explicit === 'string') {
    const p = path.resolve(crateDir, explicit, 'Cargo.toml');
    const m = parseToml(readText(p));
    return m?.workspace ? { path: p, manifest: m, self: false } : null;
  }
  let dir = path.dirname(crateDir);
  for (;;) {
    const p = path.join(dir, 'Cargo.toml');
    const text = readText(p);
    if (text !== null && /^\s*\[\s*workspace\s*[\].]/m.test(text)) {
      const m = parseToml(text);
      if (m?.workspace) {
        const rel = cleanRel(path.relative(dir, crateDir));
        const excluded = (m.workspace.exclude ?? []).some((e) => {
          const x = cleanRel(e);
          return rel === x || rel.startsWith(`${x}/`);
        });
        return excluded ? null : { path: p, manifest: m, self: false };
      }
    }
    const parent = path.dirname(dir);
    if (parent === dir) return null;
    dir = parent;
  }
}

/** Is the member at `manifestPath` listed in `manifests.ownLints` (dirs relative to the project root)? */
export function ownLintsMember(config, projectRoot, manifestPath) {
  const dir = cleanRel(path.relative(projectRoot, path.dirname(path.resolve(manifestPath))));
  return (config?.manifests?.ownLints ?? []).some((e) => globMatch(cleanRel(e), dir));
}

/**
 * Structural problems with a package manifest inside a workspace:
 * missing lint inheritance (MOD-09/ARC-01) and member profiles (PERF-09).
 * A member listed in `manifests.ownLints` may carry its own full `[lints]` table instead.
 * @param {{ ownLints?: boolean }} [opts]
 * @returns {{ id: string, msg: string, fix: string }[]}
 */
export function memberProblems(manifest, root, opts = {}) {
  const out = [];
  if (!manifest || !root) return out;
  if (manifest.package && definesWorkspaceLints(root.manifest) && !inheritsLints(manifest)) {
    const ownTable = manifest.lints !== null && typeof manifest.lints === 'object' && Object.keys(manifest.lints).length > 0;
    if (!(opts.ownLints && ownTable)) {
      out.push({
        id: 'MOD-09',
        msg: opts.ownLints
          ? 'the package is listed in manifests.ownLints but has no `[lints]` table, so clippy would accept unwrap, panic!, as casts and #[allow] in it'
          : 'the package does not inherit the workspace lints, so clippy would accept unwrap, panic!, as casts and #[allow] in it',
        fix: opts.ownLints
          ? 'give it its full, human-approved `[lints]` table (MOD-09, H-05).'
          : 'add a `[lints]` table containing only `workspace = true` (MOD-09, ARC-01). A crate that needs its own lints (an approved FFI crate) is listed by a human in `.claude/temper.json` under `manifests.ownLints`.',
      });
    }
  }
  if (!root.self && hasProfile(manifest)) {
    out.push({
      id: 'PERF-09',
      msg: 'a `[profile]` table in a member manifest is ignored by Cargo',
      fix: 'move the profile setting to the workspace root manifest, as an H-16 decision recorded in the root AGENTS.md (PERF-09).',
    });
  }
  return out;
}

/** Problems in `after` that `before` did not have, and the ones it already had (the ratchet). */
export function splitProblems(before, after) {
  const had = new Set(before.map((p) => p.id));
  return { fresh: after.filter((p) => !had.has(p.id)), legacy: after.filter((p) => had.has(p.id)) };
}

const TARGET_KEYS = ['test', 'doctest', 'harness', 'bench', 'required-features'];
const WHY = {
  target: 'this switch can stop tests, doctests or targets from running (H-12)',
  auto: 'turning off target discovery means those targets are never built or tested (H-12)',
  autobins: 'turning off binary discovery means binaries under src/bin are never built or checked (H-12)',
  build: 'it changes or disables the build script (ARC-17, H-16)',
  msrv: 'MSRV, edition and resolver changes are human decisions (H-16)',
  features: 'default features change what every user of the crate gets (API-16, H-16)',
  patch: 'it overrides where a dependency comes from (H-16)',
};

/**
 * Manifest switches whose change is a human decision, as label -> { id, value, why }.
 * H-12: switches that can stop tests, doctests or targets from running.
 * H-16: build script, dependency source overrides, MSRV/edition/resolver and default features.
 */
export function watchedKeys(m) {
  const out = new Map();
  const put = (id, why, label, v) => { if (v !== undefined) out.set(label, { id, why, value: JSON.stringify(v) }); };
  const pkg = m?.package ?? {};
  for (const k of ['autotests', 'autobenches', 'autoexamples', 'autolib']) put('H-12', WHY.auto, `package.${k}`, pkg[k]);
  put('H-12', WHY.autobins, 'package.autobins', pkg.autobins);
  put('H-16', WHY.build, 'package.build', pkg.build);
  for (const k of ['rust-version', 'edition', 'resolver']) {
    put('H-16', WHY.msrv, `package.${k}`, pkg[k]);
    put('H-16', WHY.msrv, `workspace.package.${k}`, m?.workspace?.package?.[k]);
  }
  put('H-16', WHY.msrv, 'workspace.resolver', m?.workspace?.resolver);
  put('H-16', WHY.features, 'features.default', m?.features?.default);
  put('H-16', WHY.patch, 'patch', m?.patch);
  put('H-16', WHY.patch, 'replace', m?.replace);
  const targets = [['lib', m?.lib ? [m.lib] : []]];
  for (const kind of ['bin', 'test', 'bench', 'example']) targets.push([kind, Array.isArray(m?.[kind]) ? m[kind] : []]);
  for (const [kind, list] of targets) {
    list.forEach((t, i) => {
      const head = kind === 'lib' ? '[lib]' : `[[${kind}]] ${typeof t?.name === 'string' ? t.name : `#${i + 1}`}`;
      for (const k of TARGET_KEYS) {
        if (kind === 'bench' && k === 'harness') continue; // criterion and divan need `harness = false`
        if ((kind === 'lib' || kind === 'bin') && k === 'bench') continue; // keeps libtest's bench harness out of criterion's way
        put('H-12', WHY.target, `${head} ${k}`, t?.[k]);
      }
    });
  }
  return out;
}

/**
 * Is this watched value benign in a brand-new manifest: plain inheritance from
 * the workspace, the standard's edition and resolver, or the crate's own default features?
 */
export function benignWhenNew(label, entry) {
  if (entry.value === '{"workspace":true}') return true;
  if (/(^|\.)edition$/.test(label)) return entry.value === '"2024"';
  if (/(^|\.)resolver$/.test(label)) return entry.value === '"3"';
  return label === 'features.default';
}

const DEP_KINDS = ['dependencies', 'dev-dependencies', 'build-dependencies'];

/** Every declared dependency as "kind:name" (kind: dependencies, dev-dependencies, build-dependencies). */
export function dependencyNames(m) {
  const names = new Set();
  const visit = (kind, deps) => {
    if (deps && typeof deps === 'object') for (const n of Object.keys(deps)) names.add(`${kind}:${n}`);
  };
  for (const k of DEP_KINDS) visit(k, m?.[k]);
  visit('dependencies', m?.workspace?.dependencies);
  for (const tt of Object.values(m?.target ?? {})) for (const k of DEP_KINDS) visit(k, tt?.[k]);
  return names;
}

/** The semver-compatibility class of a requirement: the major, or `0.minor` below 1.0. */
export function majorOf(req) {
  const m = /(\d+)(?:\.(\d+))?(?:\.(\d+))?/.exec(String(req));
  if (!m) return null;
  const [, a, b, c] = m;
  if (a !== '0') return a;
  if (b === undefined) return '0';
  if (b !== '0') return `0.${b}`;
  return c === undefined ? '0.0' : `0.0.${c}`;
}

/** Dependency version requirements, as "table:name" -> requirement string. */
export function depRequirements(m) {
  const out = new Map();
  const visit = (label, deps) => {
    if (!deps || typeof deps !== 'object') return;
    for (const [name, spec] of Object.entries(deps)) {
      const req = typeof spec === 'string' ? spec : (typeof spec?.version === 'string' ? spec.version : null);
      if (req !== null) out.set(`${label}:${name}`, req);
    }
  };
  for (const t of DEP_KINDS) visit(t, m?.[t]);
  visit('workspace.dependencies', m?.workspace?.dependencies);
  for (const [cfg, tt] of Object.entries(m?.target ?? {})) {
    for (const t of DEP_KINDS) visit(`target.${cfg}.${t}`, tt?.[t]);
  }
  return out;
}

/**
 * Package manifests of the workspace rooted at `rootDir`, read from the root's
 * `members` globs minus `exclude` without running cargo (the fallback when
 * cargo is unavailable). Returns null if the root manifest is unreadable.
 */
export function membersFromToml(rootDir) {
  const rootPath = path.join(rootDir, 'Cargo.toml');
  const text = readText(rootPath);
  const root = text === null ? null : parseToml(text);
  if (!root) return null;
  const out = root.package ? [rootPath] : [];
  const ws = root.workspace;
  if (!ws) return out;
  const excluded = (ws.exclude ?? []).map(cleanRel);
  for (const pattern of ws.members ?? []) {
    let dirs = [rootDir];
    for (const seg of cleanRel(pattern).split('/').filter((x) => x && x !== '.')) {
      const next = [];
      for (const d of dirs) {
        if (!/[*?]/.test(seg)) { next.push(path.join(d, seg)); continue; }
        let entries = [];
        try { entries = fs.readdirSync(d, { withFileTypes: true }); } catch { /* missing */ }
        for (const e of entries) if (e.isDirectory() && globMatch(seg, e.name)) next.push(path.join(d, e.name));
      }
      dirs = next;
    }
    for (const d of dirs) {
      const rel = cleanRel(path.relative(rootDir, d));
      if (excluded.some((x) => rel === x || rel.startsWith(`${x}/`))) continue;
      const p = path.join(d, 'Cargo.toml');
      if (fs.existsSync(p) && !out.includes(p)) out.push(p);
    }
  }
  return out;
}

/** Why a version requirement is not a caret floor (ARC-12), or null if it is one. */
export function nonCaretRequirement(req) {
  const parts = String(req).split(',').map((s) => s.trim()).filter(Boolean);
  if (!parts.length) return null;
  if (parts.some((p) => p.startsWith('='))) return 'an exact `=` pin';
  if (parts.some((p) => p.startsWith('~'))) return 'a tilde `~` requirement';
  if (parts.some((p) => p.includes('*'))) return 'a wildcard `*`';
  if (parts.some((p) => p.startsWith('<'))) return 'an upper bound `<`';
  if (parts.every((p) => p.startsWith('>'))) return 'an open-ended `>=`';
  return null;
}
