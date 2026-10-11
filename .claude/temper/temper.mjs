#!/usr/bin/env node
// temper: enforcement CLI for the rust-craft standard.
//
//   temper hook <event>        hook entry point (reads the hook JSON on stdin)
//   temper gate [--check] [--no-cache]   fmt (--check: verify only) + scan + manifests + clippy -D warnings + tests + rustdoc
//   temper scan [files..] [--all] [--json]
//   temper status              fingerprint, changed files, gate and review state
//   temper setup [--dry-run] [--force]   install repo templates (lints, rules, charters, CI) and vendor the CLI
//   temper setup --vendor-only [--dry-run]   refresh only the vendored CLI (.claude/temper/)
//   temper stats [--branch [NAME]]   enforcement telemetry summary (--branch: every session pushed to NAME)
//   temper telemetry push --session ID   push one session's telemetry to telemetry.branch (Stop runs it)
//   temper baseline [--init | --update | --tighten | --check [--base REF] [--findings-out FILE] [--json]]
//                              lint baseline for legacy code: status, record, lower, or ratchet
//   temper report [--findings FILE] [--base REF] [--out DIR] [--format json,md,html]
//                              compliance scorecard (report.json, report.md, report.html)
//
// Zero dependencies; Node 18+. Works on Windows, macOS and Linux.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import * as H from './lib/hooks.mjs';
import { discover, baseRev, rustChanges, fingerprint, relTo, crateOf, readText, headRev, resolveRev, run, safeOutputPath, findUp, norm, readTelemetry, pushTelemetry, fetchTelemetryBranch,
} from './lib/project.mjs';
import { runGate, renderGate, cachedGate, scanChanged } from './lib/gate.mjs';
import { scanSource, classifyPath, formatHit, isBuildScript } from './lib/scan.mjs';
import { verdictFor } from './lib/verdict.mjs';
import { collectFindings, cachedFindings, isComplete } from './lib/findings.mjs';
import {
  BASELINE_PATH, readBaseline, writeBaseline, baselineFrom, tighten, ratchet, renderRatchet, diffTotals, crateAliases,
} from './lib/baseline.mjs';
import { earlyNodeCheck } from './lib/requirements.mjs';

// First, before any other code: on a Node below the documented minimum, say so plainly. Where
// temper cannot run (Node 16, no structuredClone), SessionStart in a Rust workspace reports it and
// stops; on Node 17 it runs as usual and adds the warning. Other hooks and commands carry on as before.
// (Node 14 and older stop earlier still: this file's syntax needs Node 15.) When that check reads
// the hook input to find the workspace, it hands the input on in `early.stdin` for hook().
const early = await earlyNodeCheck(process.versions.node, process.argv.slice(2), process.env, { readStdin });
if (early && early.stderr) process.stderr.write(early.stderr);
if (early && early.stdout !== undefined) {
  await new Promise((done) => { process.stdout.write(early.stdout, done); });
  process.exit(early.exit);
}

const here = path.dirname(fileURLToPath(import.meta.url));
const pluginRoot = path.resolve(here, '..');

const HOOKS = {
  'session-start': H.sessionStart,
  'user-prompt': H.userPromptSubmit,
  'pre-edit': H.preEdit,
  'pre-bash': H.preBash,
  'pre-handback': H.preHandback,
  'post-tool': H.postTool,
  stop: H.stop,
  'subagent-stop': H.subagentStop,
  'subagent-start': H.subagentStart,
};

async function readStdin() {
  if (process.stdin.isTTY) return '';
  const chunks = [];
  for await (const c of process.stdin) chunks.push(c);
  return Buffer.concat(chunks).toString('utf8');
}

async function hook(event) {
  const handler = HOOKS[event];
  if (!handler) { process.stderr.write(`temper: unknown hook event ${event}\n`); return 0; }
  let input = {};
  try {
    const raw = early && early.stdin !== undefined ? early.stdin : await readStdin();
    input = JSON.parse(raw || '{}');
  } catch {
    // Same decision as before (none, exit 0), but say so: a silent pass reads as "checked and clean".
    if (process.env.TEMPER_DISABLE === '1') return 0;
    process.stdout.write(JSON.stringify({ systemMessage: `temper hook ${event}: could not parse the hook input as JSON; nothing was checked` }));
    return 0;
  }
  if (process.env.TEMPER_DISABLE === '1') return 0;
  let res;
  try {
    res = handler(input) ?? {};
  } catch (e) {
    // Never wedge the session on a hook bug: report and let the action proceed.
    process.stdout.write(JSON.stringify({ systemMessage: `temper hook ${event} failed: ${e?.message ?? e}` }));
    return 0;
  }
  if (res.json) process.stdout.write(JSON.stringify(res.json));
  else if (res.text) process.stdout.write(res.text);
  return res.exit ?? 0;
}

function project() {
  const proj = discover(process.cwd());
  if (!proj) { process.stderr.write('temper: not inside a Rust project (no Cargo.toml found)\n'); process.exit(2); }
  return proj;
}

let baseNoted = false;
function sessionBase(proj) {
  // CLI runs outside a hook: the session's base (TEMPER_BASE) when it is a commit here, else HEAD.
  const env = process.env.TEMPER_BASE;
  if (env) {
    if (resolveRev(proj, env)) return env;
    if (!baseNoted) process.stderr.write(`temper: TEMPER_BASE ${env.slice(0, 12)} is not a commit in this repository (another session's base?); using HEAD.\n`);
    baseNoted = true;
  }
  return headRev(proj);
}

function gate(args) {
  const proj = project();
  const res = runGate(proj, { base: sessionBase(proj), noCache: args.includes('--no-cache'), check: args.includes('--check') });
  if (args.includes('--json')) process.stdout.write(JSON.stringify(res, null, 2) + '\n');
  else process.stdout.write(renderGate(res) + '\n');
  return res.ok ? 0 : 1;
}

function scan(args) {
  const proj = project();
  const json = args.includes('--json');
  const files = args.filter((a) => !a.startsWith('--'));
  let results = [];
  if (args.includes('--all') || files.length) {
    const targets = files.length ? files.map((f) => path.resolve(f)) : walk(proj.root);
    for (const abs of targets) {
      const src = readText(abs);
      if (src === null) continue;
      const krate = crateOf(abs);
      const crateRel = krate ? relTo(krate.dir, abs) : relTo(proj.root, abs);
      const kind = krate ? classifyPath(crateRel, krate.hasLib) : 'prod';
      for (const h of scanSource(src, { kind, build: isBuildScript(crateRel) }).hits) results.push({ ...h, file: relTo(proj.root, abs) });
    }
  } else {
    const s = scanChanged(proj, sessionBase(proj));
    results = [...s.hard, ...s.ask, ...s.soft];
  }
  if (json) { process.stdout.write(JSON.stringify(results, null, 2) + '\n'); }
  else {
    const order = { hard: 0, ask: 1, soft: 2 };
    results.sort((a, b) => order[a.severity] - order[b.severity] || a.file.localeCompare(b.file) || a.line - b.line);
    for (const h of results) process.stdout.write(`[${h.severity}] ${formatHit(h, h.file)}\n`);
    const by = (s) => results.filter((h) => h.severity === s).length;
    process.stdout.write(`\n${by('hard')} blocking, ${by('ask')} exception request(s), ${by('soft')} advisory\n`);
  }
  return results.some((h) => h.severity === 'hard') ? 1 : 0;
}

function walk(root) {
  const out = [];
  const rec = (d) => {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      if (['target', '.git', 'node_modules', '.temper'].includes(e.name)) continue;
      const p = path.join(d, e.name);
      if (e.isDirectory()) rec(p); else if (e.name.endsWith('.rs')) out.push(p);
    }
  };
  rec(root);
  return out;
}

function status() {
  const proj = project();
  const base = sessionBase(proj);
  const changes = rustChanges(proj, base);
  const fp = fingerprint(proj, base);
  const g = cachedGate(proj, fp);
  const v = verdictFor(proj, fp);
  let bl;
  try {
    const b = readBaseline(proj.root);
    const n = b ? Object.values(b.totals).reduce((s, l) => s + Object.values(l).reduce((x, y) => x + y, 0), 0) : 0;
    bl = b ? `${BASELINE_PATH} (${n} finding(s), toolchain ${b.toolchain ?? '?'}): clippy and rustdoc ratchet` : 'none: the gate requires zero warnings';
  } catch (e) { bl = `unreadable: ${e.message}`; }
  process.stdout.write([
    `root:        ${proj.root}`,
    `state:       ${proj.stateDir}`,
    `base:        ${base ?? 'none'}`,
    `fingerprint: ${fp}`,
    `changed:     ${changes.length ? changes.join(', ') : 'none'}`,
    `gate:        ${g ? (g.ok ? 'green' : 'red') : 'not run for this fingerprint'}`,
    `review:      ${v ? `${v.verdict}${v.stale ? ' (stale)' : ''}` : 'none for this fingerprint'}`,
    `baseline:    ${bl}`,
    `config:      review=${proj.config.review} skillGate=${proj.config.skillGate}`,
  ].join('\n') + '\n');
  return 0;
}

const BASELINE_USAGE = `usage: temper baseline                 status: totals now vs the baseline, and whether it is tight
       temper baseline --init          record the current findings (refuses if the file exists)
       temper baseline --update        rewrite from the current state (may raise counts; a human decision, H-12)
       temper baseline --tighten [--any-host]
                                       lower counts to the current values (never raises); refuses
                                       on another host than the baseline's unless --any-host
       temper baseline --check [--base REF] [--findings-out FILE] [--json]
                                       ratchet: exit 0 pass, 1 fail, 2 usage error; without a
                                       baseline every finding fails (zero findings required)
options: --no-cache (collect afresh instead of reusing the gate's findings for this tree)
         --locked   pass --locked to cargo (CI: fail on a stale Cargo.lock, PROC-13)
--findings-out writes the collection and the ratchet result; the path must be under target/ or .temper/.
--init and --update refuse inside Claude Code (CLAUDECODE=1) unless TEMPER_ALLOW_RAISE=1 is set by the human.
`;

/** Parse `baseline` arguments; null on a usage error. */
function baselineArgs(args) {
  const o = { mode: 'status', base: undefined, out: null, json: false, noCache: false, locked: false, anyHost: false };
  const modes = { '--init': 'init', '--update': 'update', '--tighten': 'tighten', '--check': 'check' };
  let modeSet = false;
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    const [flag, inline] = a.startsWith('--') && a.includes('=') ? [a.slice(0, a.indexOf('=')), a.slice(a.indexOf('=') + 1)] : [a, undefined];
    const value = () => (inline !== undefined ? inline : args[++i]);
    if (modes[flag] && inline === undefined) { if (modeSet) return null; o.mode = modes[flag]; modeSet = true; }
    else if (flag === '--base') { o.base = value(); if (!o.base) return null; }
    else if (flag === '--findings-out') { o.out = value(); if (!o.out) return null; }
    else if (flag === '--json' && inline === undefined) o.json = true;
    else if (flag === '--no-cache' && inline === undefined) o.noCache = true;
    else if (flag === '--locked' && inline === undefined) o.locked = true;
    else if (flag === '--any-host' && inline === undefined) o.anyHost = true;
    else return null;
  }
  if ((o.base !== undefined || o.out) && o.mode !== 'check') return null;
  if (o.anyHost && o.mode !== 'tighten') return null;
  return o;
}

/** The findings for the current tree: the gate's cached collection when complete, else a fresh run. */
function findingsFor(proj, { noCache, locked }) {
  if (!noCache) {
    const base = sessionBase(proj);
    const c = cachedFindings(proj, fingerprint(proj, base), { base });
    if (c && isComplete(c, proj.config)) return c;
  }
  process.stderr.write('temper baseline: collecting findings (cargo clippy, cargo doc, scan)...\n');
  return collectFindings(proj, { cfg: proj.config, locked });
}

const errorLines = (c) => c.errors.slice(0, 12).map((e) => `  ${e.file ? `${e.file}:${e.line} ` : ''}${e.code ? `${e.code} ` : ''}${e.message}`).join('\n');

/** Why the collection cannot be recorded: a workspace below the git root is said plainly, compile errors listed. */
function cannotRecord(mode, c, what = 'so its lints cannot be recorded') {
  const nested = c.errors.find((e) => e.code === 'workspace-below-git-root');
  return nested ? `temper baseline --${mode}: ${nested.message}\n` : `temper baseline --${mode}: the workspace does not build, ${what}:\n${errorLines(c)}\n`;
}

const hostWarning = (mode, recorded, now) => (recorded && now && recorded !== now
  ? `temper baseline --${mode}: warning: the baseline was recorded on ${recorded} and this is ${now}. Findings under cfg(target_os/unix/windows) differ by platform: record and tighten the baseline on the platform CI uses.\n`
  : '');

function baseline(args) {
  const o = baselineArgs(args);
  if (!o) { process.stderr.write(BASELINE_USAGE); return 2; }
  const proj = project();
  let current;
  try { current = readBaseline(proj.root); } catch (e) { process.stderr.write(`temper baseline: ${e.message}\n`); return 2; }
  const where = BASELINE_PATH;

  if (o.mode === 'init' && current) {
    process.stderr.write(`temper baseline --init: ${where} already exists. Use --tighten to lower it; --update rewrites it (a human decision).\n`);
    return 2;
  }
  if (o.mode === 'tighten' && !current) {
    process.stderr.write(`temper baseline --tighten: no ${where}. Without a baseline the gate and CI require zero warnings; recording one is \`temper baseline --init\` (a human decision).\n`);
    return 2;
  }
  if ((o.mode === 'init' || o.mode === 'update') && process.env.CLAUDECODE === '1' && process.env.TEMPER_ALLOW_RAISE !== '1') {
    process.stderr.write(`temper baseline --${o.mode}: refused inside a Claude Code session. Recording legacy debt weakens verification (H-12), so it is the human's decision: run it in your own terminal, or approve \`TEMPER_ALLOW_RAISE=1 node <temper.mjs> baseline --${o.mode}\` when the agent asks.\n`);
    return 2;
  }
  let out = null;
  if (o.out) {
    out = safeOutputPath(proj.root, o.out);
    if (!out) { process.stderr.write(`temper baseline: --findings-out ${o.out} refused: write findings under target/ or .temper/ (no \`..\`, no symlink anywhere on the path).\n`); return 2; }
  }
  let base = null;
  if (o.mode === 'check') {
    if (o.base !== undefined) {
      if (!resolveRev(proj, o.base)) { process.stderr.write(`temper baseline --check: --base ${o.base} is not a commit in this repository (a typo, or a shallow clone: fetch it first).\n`); return 2; }
      base = o.base;
    } else if (process.env.TEMPER_BASE) base = sessionBase(proj); // an unresolvable session base becomes HEAD, with a note
  }
  if ((o.mode === 'init' || o.mode === 'update') && !fs.existsSync(path.join(proj.root, 'Cargo.toml'))) {
    const at = findUp(process.cwd(), (d) => fs.existsSync(path.join(d, 'Cargo.toml')));
    process.stderr.write(`temper baseline --${o.mode}: temper needs the Cargo workspace at the git root: ${proj.root} has no Cargo.toml${at ? ` (the workspace is at ${relTo(proj.root, at)})` : ''}. Nested workspaces are not supported yet.\n`);
    return 2;
  }
  if (o.mode === 'init' || o.mode === 'update') {
    // Recording debt on unformatted code would make the first `cargo fmt` (the gate runs it) touch legacy lines.
    const fmt = run('cargo', ['fmt', '--all', '--', '--check'], { cwd: proj.root, env: { CARGO_TERM_COLOR: 'never' }, timeoutMs: 300000 });
    if (fmt.code !== 0) {
      const files = [...new Set((fmt.stdout.match(/^Diff in (.+?)(?: at line \d+|:\d+)/gm) ?? []).map((l) => l.replace(/^Diff in /, '').replace(/( at line \d+|:\d+:?)$/, '')))];
      process.stderr.write(`temper baseline --${o.mode}: \`cargo fmt --all -- --check\` fails${files.length ? ` (${files.length} file(s), e.g. ${relTo(proj.root, files[0])})` : fmt.error ? ` (${fmt.error.message})` : ''}. Run \`cargo fmt --all\` and commit the formatting on its own first, then record the baseline; otherwise the gate's first \`cargo fmt\` touches legacy lines and they count as new code.\n`);
      return 1;
    }
  }
  const collection = o.mode === 'check' ? collectFindings(proj, { cfg: proj.config, locked: o.locked }) : findingsFor(proj, o);

  if (o.mode === 'check') {
    const res = ratchet(collection, current, { proj, base });
    if (out) {
      try {
        fs.mkdirSync(path.dirname(out), { recursive: true });
        fs.writeFileSync(out, `${JSON.stringify({ ...collection, ratchet: res }, null, 2)}\n`);
      } catch (e) { process.stderr.write(`temper baseline: cannot write ${o.out}: ${e.message}\n`); return 2; }
    }
    if (o.json) process.stdout.write(`${JSON.stringify(res, null, 2)}\n`);
    else process.stdout.write(`temper baseline --check: ${res.ok ? 'PASS' : 'FAIL'}\n${renderRatchet(res)}\n`);
    return res.ok ? 0 : 1;
  }

  if (o.mode === 'init' || o.mode === 'update') {
    if (collection.errors.length) { process.stderr.write(cannotRecord(o.mode, collection)); return 1; }
    process.stderr.write(hostWarning(o.mode, current?.host, collection.host));
    const next = baselineFrom(collection, { created: current?.created });
    writeBaseline(proj.root, next);
    const total = Object.values(next.totals).reduce((n, l) => n + Object.values(l).reduce((a, b) => a + b, 0), 0);
    const lines = [`temper baseline --${o.mode}: wrote ${where}: ${total} finding(s) in ${Object.keys(next.totals).length} crate(s) (toolchain ${next.toolchain ?? 'unknown'}, host ${next.host ?? 'unknown'}).`];
    if (current) for (const d of diffTotals(current, next)) lines.push(`  ${d.crate} ${d.lint}: ${d.from} -> ${d.to}`);
    lines.push('Commit it with the change that needs it. The gate and CI now fail only on new findings and count increases.');
    process.stdout.write(`${lines.join('\n')}\n`);
    return 0;
  }

  if (o.mode === 'tighten') {
    if (collection.errors.length) { process.stderr.write(cannotRecord('tighten', collection, 'and a crate that does not compile reports no lints, so nothing was lowered')); return 1; }
    if (current.host && collection.host && current.host !== collection.host && !o.anyHost) {
      process.stderr.write(`temper baseline --tighten: refused: the baseline was recorded on ${current.host} and this is ${collection.host}. Code under cfg(target_os/unix/windows) lints differently, so counts lowered here can fail CI on lines nobody touched. Tighten on the platform CI uses, or pass --any-host if this workspace has no platform-specific code.\n`);
      return 2;
    }
    process.stderr.write(hostWarning('tighten', current.host, collection.host));
    const next = tighten(current, collection, { proj });
    const changes = diffTotals(current, next);
    if (!changes.length) { process.stdout.write(`temper baseline --tighten: ${where} is already tight.\n`); return 0; }
    writeBaseline(proj.root, next);
    process.stdout.write(`temper baseline --tighten: lowered ${changes.length} count(s) in ${where}:\n${changes.map((d) => `  ${d.crate} ${d.lint}: ${d.from} -> ${d.to}`).join('\n')}\n`);
    return 0;
  }

  // status
  if (!current) {
    const n = collection.findings.filter((f) => !f.exception).length;
    process.stdout.write(`temper baseline: no ${where}; the gate and CI require zero warnings. ${n} finding(s) now${collection.errors.length ? `, ${collection.errors.length} compile error(s)` : ''}.\n`);
    return 0;
  }
  const res = ratchet(collection, current, { proj, base: null });
  const alias = crateAliases(current, collection.crates);
  const per = {};
  for (const [c, lints] of Object.entries(current.totals)) for (const n of Object.values(lints)) { per[c] ??= { b: 0, n: 0 }; per[c].b += n; }
  for (const f of collection.findings.filter((x) => !x.exception)) { const c = alias.get(f.crate) ?? f.crate ?? '(none)'; per[c] ??= { b: 0, n: 0 }; per[c].n += 1; }
  const width = Math.max(5, ...Object.keys(per).map((c) => c.length));
  const rows = Object.keys(per).sort().map((c) => `  ${c.padEnd(width)}  ${String(per[c].b).padStart(8)}  ${String(per[c].n).padStart(6)}`);
  let tight = 'tight: yes';
  if (res.fixed.length) tight = res.ok ? `tight: no; ${res.fixed_count} fixed since the baseline: run temper baseline --tighten` : `tight: no; ${res.fixed_count} fixed since the baseline (tighten once the counts above are fixed)`;
  process.stdout.write([
    `temper baseline: ${where} (created ${current.created ?? '?'}, updated ${current.updated ?? '?'}, toolchain ${current.toolchain ?? '?'}, host ${current.host ?? '?'})`,
    `  ${'crate'.padEnd(width)}  baseline     now`,
    ...rows,
    `  ${'total'.padEnd(width)}  ${String(res.baseline_total).padStart(8)}  ${String(res.legacy_total).padStart(6)}`,
    tight,
    res.increases.length ? `above the baseline:\n${res.increases.map((x) => `  ${x.crate} ${x.lint}: baseline ${x.baseline}, now ${x.current}`).join('\n')}` : '',
    ...res.notes.map((n) => `note: ${n}`),
    collection.errors.length ? `compile errors:\n${errorLines(collection)}` : '',
  ].filter(Boolean).join('\n') + '\n');
  return 0;
}

/** Load a module a separate change provides (report, vendoring); null with a message when this copy lacks it. */
async function optionalModule(rel, what, { quiet = false } = {}) {
  try {
    return await import(rel);
  } catch (e) {
    if (e?.code === 'ERR_MODULE_NOT_FOUND' && norm(String(e.message)).includes(rel.replace('./', ''))) {
      if (!quiet) process.stderr.write(`temper: ${what} is not available in this copy of temper (${rel.replace('./', 'scripts/')} is missing).\n`);
      return null;
    }
    throw e;
  }
}

async function report(args) {
  const mod = await optionalModule('./lib/report.mjs', '`temper report`');
  if (!mod) return 2;
  return mod.runReport(args);
}

async function setup(args) {
  const dry = args.includes('--dry-run');
  const force = args.includes('--force');
  const root = discover(process.cwd())?.root ?? process.cwd();
  if (args.includes('--vendor-only')) return setupVendorOnly(root, dry);
  const tpl = path.join(pluginRoot, 'templates', 'workspace');
  const actions = [];
  const copy = (srcRel, dstRel = srcRel) => {
    const src = path.join(tpl, srcRel);
    const dst = path.join(root, dstRel);
    const exists = fs.existsSync(dst);
    if (exists && !force) { actions.push(`skip   ${dstRel} (exists; compare with ${path.relative(root, src)})`); return; }
    actions.push(`${exists ? 'update' : 'create'} ${dstRel}`);
    if (!dry) { fs.mkdirSync(path.dirname(dst), { recursive: true }); fs.copyFileSync(src, dst); }
  };
  const list = (d, base = '') => fs.readdirSync(path.join(d, base), { withFileTypes: true }).flatMap((e) => {
    const r = path.join(base, e.name);
    return e.isDirectory() ? list(d, r) : [r];
  });
  for (const f of list(tpl)) {
    if (f === 'Cargo.toml') continue; // merged by hand: never overwrite a manifest
    copy(f);
  }
  const manifest = path.join(root, 'Cargo.toml');
  if (fs.existsSync(manifest)) {
    const t = readText(manifest) ?? '';
    if (!/\[workspace\.lints/.test(t)) actions.push(`manual Cargo.toml: merge [workspace.lints.*] and [workspace.package] from ${path.join(tpl, 'Cargo.toml')}, then add \`[lints] workspace = true\` to each member`);
  } else {
    copy('Cargo.toml');
  }
  // An existing clippy.toml stays the repository's, but without the template's test exemptions
  // the restriction lints fire in test code, and the baseline records that as debt.
  if (!force) {
    const have = readText(path.join(root, 'clippy.toml'));
    if (have !== null) {
      const missing = (readText(path.join(tpl, 'clippy.toml')) ?? '').split('\n')
        .map((l) => l.replace(/#.*/, '').trim())
        .filter((l) => /^allow-[a-z-]+-in-tests\s*=/.test(l))
        .filter((l) => !new RegExp(`^\\s*${l.split('=')[0].trim()}\\s*=`, 'm').test(have));
      if (missing.length) actions.push(`manual clippy.toml: add ${missing.join(', ')}; without them the restriction lints fire in test code and the baseline counts it as debt`);
    }
  }
  for (const [rel, lines] of strictLints(root, tpl)) {
    actions.push(`manual ${rel}:${lines.join(',')}: denies every warning, which fails on each legacy finding once the lint tables are merged; lint with \`temper baseline --check\`, and build and document without -D warnings`);
  }
  const vendor = await optionalModule('./lib/vendor.mjs', 'vendoring', { quiet: true });
  if (vendor) {
    const v = vendor.vendorTool(root, { dry, force });
    actions.push(...(v?.actions ?? []));
  } else {
    actions.push('skip   .claude/temper/ (this temper copy cannot vendor itself: scripts/lib/vendor.mjs is missing)');
  }
  // Scratch state and cargo's build output stay out of git. Cargo.lock is not added: it is committed.
  const gi = path.join(root, '.gitignore');
  let giText = readText(gi) ?? '';
  for (const [line, present] of [['.temper/', /^\/?\.temper\/?$/m], ['/target/', /^\/?target\/?$/m]]) {
    if (present.test(giText)) continue;
    actions.push(`append .gitignore: ${line}`);
    const add = `${giText.endsWith('\n') || giText === '' ? '' : '\n'}${line}\n`;
    if (!dry) fs.appendFileSync(gi, add);
    giText += add;
  }
  process.stdout.write(`${dry ? '[dry run] ' : ''}temper setup in ${root}\n${actions.map((a) => `  ${a}`).join('\n')}\n`);
  process.stdout.write('\nNext: give every crate a charter (copy templates/crate/AGENTS.md and CLAUDE.md into each crate root and fill it in).\n');
  process.stdout.write('Before the first push, run `cargo machete`: rust.yml fails on an unused dependency.\n');
  return 0;
}

/**
 * `setup --vendor-only`: replace `.claude/temper/` with this temper's CLI, as an upgrade does, and
 * nothing else. The templates, the lint tables and `.gitignore` are left as they are.
 */
async function setupVendorOnly(root, dry) {
  const vendor = await optionalModule('./lib/vendor.mjs', '`temper setup --vendor-only`');
  if (!vendor) return 2;
  const { actions } = vendor.vendorTool(root, { dry, force: true });
  process.stdout.write(`${dry ? '[dry run] ' : ''}temper setup --vendor-only in ${root}\n${actions.map((a) => `  ${a}`).join('\n')}\n`);
  return 0;
}

/**
 * Lines in CI workflows and contributor docs that deny every warning (`-D warnings`), as
 * [path, line numbers] pairs. A file identical to temper's template is skipped: its strict steps
 * run only without the vendored CLI.
 */
function strictLints(root, tpl) {
  const wf = path.join('.github', 'workflows');
  let workflows = [];
  try {
    workflows = fs.readdirSync(path.join(root, wf)).filter((f) => /\.ya?ml$/.test(f)).map((f) => path.join(wf, f));
  } catch (e) {
    if (e?.code !== 'ENOENT' && e?.code !== 'ENOTDIR') throw e;
  }
  const docs = ['CLAUDE.md', 'AGENTS.md', 'CONTRIBUTING.md', path.join('.github', 'pull_request_template.md'), path.join('.github', 'PULL_REQUEST_TEMPLATE.md')];
  const hits = [];
  for (const rel of [...workflows, ...docs]) {
    const text = readText(path.join(root, rel));
    if (text === null || text === readText(path.join(tpl, rel))) continue;
    const lines = text.split('\n').flatMap((l, i) => (/-D ?warnings\b/.test(l) ? [i + 1] : []));
    if (lines.length) hits.push([rel.split(path.sep).join('/'), lines]);
  }
  return hits;
}

const pct = (sorted, p) => sorted[Math.min(sorted.length - 1, Math.floor((sorted.length * p) / 100))];
const secs = (ms) => `${(ms / 1000).toFixed(1)} s`;
const STOP_OUTCOMES = new Set(['stop-green', 'stop-blocked', 'stop-allowed-red', 'stop-allowed-unreviewed', 'gate-unavailable']);

function stats(args) {
  const proj = project();
  const i = args.indexOf('--branch');
  let rows;
  if (i >= 0) {
    const named = args[i + 1] && !args[i + 1].startsWith('--') ? args[i + 1] : null;
    const branch = named ?? proj.config.telemetry?.branch;
    if (!branch) { process.stderr.write('temper stats --branch: name the branch, or set telemetry.branch in .claude/temper.json\n'); return 2; }
    rows = fetchTelemetryBranch(proj, branch);
    if (rows === null) { process.stderr.write(`temper stats: could not fetch the ${branch} branch from origin\n`); return 1; }
  } else {
    rows = readTelemetry(proj);
  }
  const count = {};
  for (const r of rows) count[r.event] = (count[r.event] ?? 0) + 1;
  const rules = {};
  for (const r of rows.filter((x) => x.event === 'deny-scan')) for (const id of r.rules ?? []) rules[id] = (rules[id] ?? 0) + 1;
  const bySession = new Map();
  for (const r of rows) bySession.set(r.sid, [...(bySession.get(r.sid) ?? []), r]);
  const out = [`sessions: ${bySession.size}`];
  if (rows.length) out.push(`from ${rows.map((r) => r.t).sort()[0]} to ${rows.map((r) => r.t).sort().at(-1)}`);
  out.push(`events:\n${Object.entries(count).sort((a, b) => b[1] - a[1]).map(([k, v]) => `  ${k.padEnd(26)} ${v}`).join('\n')}`);
  if (Object.keys(rules).length) out.push(`escape hatches blocked by rule:\n${Object.entries(rules).sort((a, b) => b[1] - a[1]).map(([k, v]) => `  ${k.padEnd(8)} ${v}`).join('\n')}`);
  // Gate latency at Stop (older rows carry no ms). A gate-start that the same session and agent
  // never follows with an outcome was a hook killed mid-gate, most likely by its timeout.
  const fresh = rows.filter((r) => STOP_OUTCOMES.has(r.event) && typeof r.ms === 'number' && !r.cached).map((r) => r.ms).sort((a, b) => a - b);
  const cached = rows.filter((r) => STOP_OUTCOMES.has(r.event) && r.cached === true).length;
  let killed = 0;
  for (const list of bySession.values()) {
    const pending = new Set();
    for (const r of list) {
      const who = r.agent ?? 'main';
      if (r.event === 'gate-start') { if (pending.has(who)) killed += 1; pending.add(who); }
      else if (STOP_OUTCOMES.has(r.event)) pending.delete(who);
    }
    killed += pending.size;
  }
  if (fresh.length || cached || killed) {
    out.push(`gate at Stop: ${fresh.length} run(s), ${cached} cached${fresh.length ? `; p50 ${secs(pct(fresh, 50))}, p90 ${secs(pct(fresh, 90))}, max ${secs(fresh.at(-1))}` : ''}${killed ? `; ${killed} cut short (hook timeout?)` : ''}`);
  }
  // Review loop: verdicts per session, and how many sessions were approved on the first verdict.
  const rounds = [...bySession.values()].map((list) => list.filter((r) => r.event === 'verdict')).filter((v) => v.length);
  if (rounds.length) {
    const n = rounds.map((v) => v.length).sort((a, b) => a - b);
    const first = rounds.filter((v) => v[0].verdict === 'approved').length;
    out.push(`review loop: ${rounds.length} session(s) with verdicts; rounds per session p50 ${pct(n, 50)}, max ${n.at(-1)}; approved on the first verdict in ${first}`);
  }
  process.stdout.write(`${out.join('\n')}\n`);
  return 0;
}

function telemetryCmd(args) {
  const i = args.indexOf('--session');
  const sid = i >= 0 ? args[i + 1] : null;
  if (args[0] !== 'push' || !sid) { process.stderr.write('usage: temper telemetry push --session ID\n'); return 2; }
  const res = pushTelemetry(project(), sid);
  process.stdout.write(res.pushed ? `pushed ${res.rows} row(s) to ${res.branch}:${res.file}\n` : `not pushed: ${res.why}\n`);
  return 0;
}

// A closed pipe (`temper scan | head`) ends output quietly instead of crashing.
process.stdout.on('error', (e) => { if (e.code === 'EPIPE') process.exit(process.exitCode ?? 0); else throw e; });

const [cmd, ...rest] = process.argv.slice(2);
const code = await (async () => {
  switch (cmd) {
    case 'hook': return hook(rest[0]);
    case 'gate': return gate(rest);
    case 'scan': return scan(rest);
    case 'status': return status();
    case 'setup': return setup(rest);
    case 'stats': return stats(rest);
    case 'telemetry': return telemetryCmd(rest);
    case 'baseline': return baseline(rest);
    case 'report': return report(rest);
    default:
      process.stdout.write('usage: temper <hook EVENT | gate [--check] [--no-cache] [--json] | scan [FILES..] [--all] [--json] | status | setup [--dry-run] [--force | --vendor-only] | stats [--branch [NAME]] | telemetry push --session ID\n'
        + '              | baseline [--init | --update | --tighten | --check [--base REF] [--findings-out FILE] [--json]]\n'
        + '              | report [--findings FILE] [--base REF] [--out DIR] [--format json,md,html]>\n');
      return cmd ? 2 : 0;
  }
})();
process.exitCode = code;
