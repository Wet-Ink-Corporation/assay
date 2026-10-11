// Vendoring: copy the temper CLI (scripts/temper.mjs and scripts/lib/**,
// rulemap.json included) into a repository's .claude/temper/, with a VERSION
// file, so CI and the pre-commit hook run `node .claude/temper/temper.mjs ...`
// without the plugin installed. The CLI has no dependencies, so a plain copy
// runs. Only `temper setup` writes the copy; hooks never do.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const scriptsDir = path.resolve(here, '..');

/** Where the copy lives, relative to the repository root. */
export const VENDOR_DIR = '.claude/temper';

const read = (p) => { try { return fs.readFileSync(p, 'utf8'); } catch { return null; } };

/**
 * The temper version this code belongs to: `VERSION` next to temper.mjs in a
 * vendored copy, otherwise the plugin's `.claude-plugin/plugin.json`.
 */
export function temperVersion() {
  const v = read(path.join(scriptsDir, 'VERSION'))?.trim();
  if (v) return v;
  try { return JSON.parse(read(path.join(scriptsDir, '..', '.claude-plugin', 'plugin.json'))).version ?? 'unknown'; } catch { return 'unknown'; }
}

/** Relative (forward-slash) paths of the files under `dir`, sorted; skips dotfiles and temp files. */
function walk(dir, keepAll = false) {
  const out = [];
  const rec = (d, base) => {
    let entries = [];
    try { entries = fs.readdirSync(d, { withFileTypes: true }); } catch { return; }
    for (const e of entries) {
      if (!keepAll && (e.name.startsWith('.') || e.name.endsWith('.tmp') || e.name === 'node_modules')) continue;
      const rel = base ? `${base}/${e.name}` : e.name;
      if (e.isDirectory()) rec(path.join(d, e.name), rel);
      else if (e.isFile()) out.push(rel);
    }
  };
  rec(dir, '');
  return out.sort();
}

/** What the copy must contain: relative path -> bytes. */
function sourceFiles() {
  const files = new Map();
  files.set('temper.mjs', fs.readFileSync(path.join(scriptsDir, 'temper.mjs')));
  for (const rel of walk(path.join(scriptsDir, 'lib'))) files.set(`lib/${rel}`, fs.readFileSync(path.join(scriptsDir, 'lib', rel)));
  files.set('VERSION', Buffer.from(`${temperVersion()}\n`));
  return files;
}

// A checkout with core.autocrlf turns LF into CRLF in both trees; that is not drift.
const sameText = (a, b) => a.equals(b) || a.toString('utf8').replace(/\r\n/g, '\n') === b.toString('utf8').replace(/\r\n/g, '\n');

function listed(items, max = 6) {
  return items.length > max ? `${items.slice(0, max).join(', ')} and ${items.length - max} more` : items.join(', ');
}

/**
 * Install or refresh `<root>/.claude/temper/`. A missing copy is created. An
 * existing copy that differs from this temper is drift: reported, and replaced
 * only with `force` (files the plugin no longer ships are removed). `dry`
 * reports without writing.
 * @param {string} root repository root
 * @param {{ dry?: boolean, force?: boolean }} [opts]
 * @returns {{ actions: string[], drift: boolean }} actions in `temper setup` style; drift: the existing copy differed
 */
export function vendorTool(root, { dry = false, force = false } = {}) {
  const dest = path.join(root, ...VENDOR_DIR.split('/'));
  if (path.resolve(dest) === scriptsDir) return { actions: [`skip   ${VENDOR_DIR}/ (this is the vendored copy itself)`], drift: false };
  const version = temperVersion();
  const src = sourceFiles();
  const write = (rels) => {
    for (const rel of rels) {
      const p = path.join(dest, ...rel.split('/'));
      fs.mkdirSync(path.dirname(p), { recursive: true });
      fs.writeFileSync(p, src.get(rel));
    }
  };
  if (!fs.existsSync(dest)) {
    if (!dry) write([...src.keys()]);
    const libCount = [...src.keys()].filter((k) => k.startsWith('lib/')).length;
    return { actions: [`create ${VENDOR_DIR}/ (temper ${version}: temper.mjs, lib/ with ${libCount} files, VERSION)`], drift: false };
  }
  const have = new Set(walk(dest, true));
  const changed = [];
  const missing = [];
  for (const [rel, bytes] of src) {
    if (!have.has(rel)) missing.push(rel);
    else if (!sameText(fs.readFileSync(path.join(dest, ...rel.split('/'))), bytes)) changed.push(rel);
  }
  const extra = [...have].filter((rel) => !src.has(rel));
  if (!changed.length && !missing.length && !extra.length) {
    return { actions: [`skip   ${VENDOR_DIR}/ (up to date with temper ${version})`], drift: false };
  }
  const what = listed([
    ...changed.map((f) => `${f} (changed)`), ...missing.map((f) => `${f} (missing)`), ...extra.map((f) => `${f} (not in temper ${version})`),
  ]);
  if (!force) {
    const was = read(path.join(dest, 'VERSION'))?.trim();
    return {
      actions: [`drift  ${VENDOR_DIR}/ differs from temper ${version}${was ? ` (the copy says ${was})` : ''}: ${what}; rerun with --vendor-only to replace only the vendored CLI (or --force to also overwrite the other template files; an existing Cargo.toml is never overwritten)`],
      drift: true,
    };
  }
  if (!dry) {
    write([...changed, ...missing]);
    for (const rel of extra) fs.rmSync(path.join(dest, ...rel.split('/')), { force: true });
  }
  return { actions: [`update ${VENDOR_DIR}/ to temper ${version}: ${what}`], drift: true };
}
