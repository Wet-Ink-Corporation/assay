// Hook handlers. Each returns { json?, text?, exit? }.
// Context strings are phrased as facts about the project, not as out-of-band
// commands, so Claude treats them as project information.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  discover, relTo, norm, readText, writeAtomic, globMatch, crateOf, sessionDir, agentKey,
  markSkillLoaded, skillLoaded, setBase, baseRev, pruneSessions, pruneMarkers, telemetry, rustChanges,
  fingerprint, diffStat, run, startFingerprint, pruneFingerprintState,
} from './project.mjs';
import { scanSource, newHits, classifyPath, formatHit, isBuildScript } from './scan.mjs';
import { parseCommands, commandWrites, patchCommand, patchFiles, wordPath, landing, resolvePath as rsResolvePath } from './shell.mjs';
import { parseToml } from './toml.mjs';
import {
  workspaceRootFor, memberProblems, watchedKeys, benignWhenNew, depRequirements, nonCaretRequirement, inheritsLints,
  ownLintsMember, splitProblems, dependencyNames, majorOf,
} from './manifest.mjs';
import { runtimeWarnings } from './requirements.mjs';
import { runGate, renderGate, cachedGate } from './gate.mjs';
import { writeTargets, msysPath } from './shell.mjs';
import { extractVerdict, validateVerdict, recordVerdict, verdictFor, summarizeFindings } from './verdict.mjs';
import {
  parseCommands as protectedCommands, commandWrites as protectedWrites, patchCommand as protectedPatch, pathspecPattern, resolvePath,
  landing as protectedLanding, patchFiles as protectedPatchFiles,
} from './shell.mjs';

const REVIEWER = /(^|:)rust-reviewer$/;
const IMPLEMENTER = /(^|:)rust-implementer$/;
const PRELOADED = /(^|:)rust-(implementer|reviewer|architect)$/;
const SKILL = /(^|:)rust-craft$/;

const pluginRoot = () => process.env.CLAUDE_PLUGIN_ROOT ?? path.resolve(path.dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1')), '..', '..');
const cli = () => path.join(pluginRoot(), 'scripts', 'temper.mjs');
const gateCmd = () => `node "${cli()}" gate`;

const ctx = (event, text) => ({ json: { hookSpecificOutput: { hookEventName: event, additionalContext: text } } });
const deny = (reason, extraContext) => ({ json: { hookSpecificOutput: { hookEventName: 'PreToolUse', permissionDecision: 'deny', permissionDecisionReason: reason, ...(extraContext ? { additionalContext: extraContext } : {}) } } });
const ask = (reason, extraContext) => ({ json: { hookSpecificOutput: { hookEventName: 'PreToolUse', permissionDecision: 'ask', permissionDecisionReason: reason, ...(extraContext ? { additionalContext: extraContext } : {}) } } });
const block = (reason) => ({ json: { decision: 'block', reason } });
const notice = (msg) => ({ json: { systemMessage: msg } });
const none = () => ({});

// Compact form of the Iron rules table in skills/rust-craft/SKILL.md; keep the two in step.
export const IRON_RULES = [
  'H-01 no unsafe',
  'H-02 no unwrap/expect in production code or doctests',
  'H-03 no panic!/todo!/unimplemented!/unreachable!, and no std call that panics on input you do not control (integer / and %, thread::spawn, env::args, Duration::from_secs_f64, ...)',
  'H-04 no panicking index/slice on unproven bounds, including map[&k] and deque[i]',
  'H-05 no #[allow]; #[expect(lint, reason)] needs human approval; never weaken lint/CI/hook config',
  'H-06 no dbg!, no print macros in production code (binaries: writeln! on a locked handle), no commented-out code, no untracked TODOs',
  'H-07 no Box::leak/mem::forget/transmute/static mut/\'static-to-silence-lifetimes; shared mutable state is justified',
  'H-08 no clones to appease the borrow checker',
  'H-09 no String/Box<dyn Error>/anyhow errors in public library signatures',
  'H-10 no silently discarded failures (let _ =, .ok();, empty Err arms, masking defaults)',
  'H-11 no lossy `as` numeric casts',
  'H-12 never weaken tests or checks to get green: no deleted or #[ignore]d tests, --no-verify, --ignore-rust-version or manifest switches that stop tests (test, doctest, harness, autotests)',
  'H-13 bounded channels/buffers, timeouts, owned tasks and threads',
  'H-14 no blocking in async; no lock guard, RefCell borrow, entered span or watch::Ref across .await; Drop never blocks',
  'H-15 zero warnings; public items documented',
  'H-16 new deps, major bumps, [patch], rust-version raises, API breaks, exceptions, crate boundaries, migrations, test rewrites are human decisions',
];

function discoverFor(input, file) {
  if (file) {
    let dir = path.dirname(path.resolve(file));
    while (!fs.existsSync(dir)) { const p = path.dirname(dir); if (p === dir) break; dir = p; }
    const proj = discover(dir);
    if (proj) return proj;
  }
  return discover(input.cwd);
}

/**
 * SessionStart output plus a line when Node is older than the documented minimum (README,
 * Install), for the human (systemMessage) and the model (additionalContext). Message only.
 */
export function withRuntimeWarnings(out, warnings = runtimeWarnings({ node: process.versions.node })) {
  if (!warnings.length) return out;
  const hso = out.json.hookSpecificOutput;
  const extra = warnings.map((w) => `- ${w}`).join('\n');
  return { json: { ...out.json, systemMessage: warnings.join('\n'), hookSpecificOutput: { ...hso, additionalContext: `${hso.additionalContext}\n${extra}` } } };
}

// ---------------------------------------------------------------- SessionStart

export function sessionStart(input) {
  const proj = discover(input.cwd);
  if (!proj) return none();
  const sid = input.session_id;
  const fresh = ['startup', 'clear', 'fork'].includes(input.source);
  setBase(proj, sid, fresh);
  if (input.source === 'startup') { pruneSessions(proj); pruneMarkers(); pruneFingerprintState(proj, startFingerprint(proj, sid)); }
  if (process.env.CLAUDE_ENV_FILE) {
    const baseLine = baseRev(proj, sid) ? `export TEMPER_BASE="${baseRev(proj, sid)}"\n` : '';
    try { fs.appendFileSync(process.env.CLAUDE_ENV_FILE, `export TEMPER_CLI="${norm(cli())}"\n${baseLine}`); } catch { /* ignore */ }
  }
  const base = baseRev(proj, sid);
  const changes = rustChanges(proj, base);
  let state = 'no Rust changes since the session base';
  if (changes.length) {
    const fp = fingerprint(proj, base);
    const g = cachedGate(proj, fp);
    const v = verdictFor(proj, fp);
    state = `${changes.length} Rust file(s) changed; gate ${g ? (g.ok ? 'green' : 'red') : 'not run'}; review ${v ? v.verdict : 'none'} (fingerprint ${fp})`;
  }
  telemetry(proj, input, 'session-start', { source: input.source });
  const loaded = skillLoaded(proj, input);
  const text = [
    'This repository is a Rust workspace governed by the temper plugin and the rust-craft engineering standard.',
    `- The standard is the skill \`temper:rust-craft\`. It ${loaded ? 'is loaded in this session' : 'has not been loaded in this session yet'}. Edits to .rs and Cargo.toml files are refused until it has been loaded in the current agent context.`,
    `- Iron rules (production code): ${IRON_RULES.join('; ')}.`,
    '- Workflow: orient, frame, plan (.temper/plans/), red, green, refactor, self-review, gate, independent review by the temper:rust-reviewer subagent, then report with the Status block.',
    `- Gate command: ${gateCmd()} (also available as: node "$TEMPER_CLI" gate). It runs cargo fmt, the escape-hatch scan, the member-manifest check, clippy -D warnings, tests and rustdoc -D warnings, cached per tree fingerprint. When .claude/temper-baseline.json exists, clippy and rustdoc findings ratchet against it: legacy debt passes; findings on changed lines and counts above the baseline fail.`,
    '- A turn that changed Rust ends when the gate is green and temper:rust-reviewer has approved the current tree. To pause instead, the final message carries a line "Status: WIP" or "Status: BLOCKED" with the reason or question.',
    '- Edits to lint, format, toolchain, CI, hook and agent configuration, new dependencies, lint exceptions and removed tests are routed to the human for approval.',
    `- Session base revision: ${base ?? 'none (no git)'}. Current state: ${state}.`,
  ].join('\n');
  return withRuntimeWarnings(ctx('SessionStart', text));
}

// ------------------------------------------------------------ UserPromptSubmit

export function userPromptSubmit(input) {
  if (skillLoaded(null, input)) return none();
  const proj = discover(input.cwd);
  if (!proj || !proj.config.skillGate || skillLoaded(proj, input)) return none();
  return ctx('UserPromptSubmit', 'temper: this is a Rust workspace and the rust-craft standard (skill `temper:rust-craft`) is not loaded in this session yet; Rust edits are refused until it is. Planning Rust work benefits from loading it first.');
}

// ------------------------------------------------------------------ PreToolUse

export function applyEdit(before, ti, tool) {
  if (tool === 'Write') return ti.content ?? '';
  const edits = tool === 'MultiEdit' ? (ti.edits ?? []) : [ti];
  let text = before;
  for (const e of edits) {
    if (typeof e.old_string !== 'string' || !text.includes(e.old_string)) return null;
    text = e.replace_all ? text.split(e.old_string).join(e.new_string ?? '') : text.replace(e.old_string, () => e.new_string ?? '');
  }
  return text;
}

/** Text of Cargo.toml sections that control lints and profiles. */
export function lintSections(toml) {
  const out = [];
  let keep = false;
  for (const line of toml.split(/\r?\n/)) {
    const h = line.match(/^\s*\[([^\]]+)\]\s*$/);
    if (h) keep = /(^|\.)lints(\.|$)|^profile(\.|$)/.test(h[1].trim());
    if (keep) out.push(line.trim());
  }
  return out.filter(Boolean).join('\n');
}

/**
 * Do the lint or profile settings differ? Adding or keeping a bare
 * `[lints] workspace = true` (plain inheritance) is not a change of settings.
 */
export function lintConfigChanged(before, after) {
  const pb = before === '' ? {} : parseToml(before);
  const pa = parseToml(after);
  if (!pa || !pb) return lintSections(before) !== lintSections(after);
  const pick = (m) => JSON.stringify({
    lints: inheritsLints(m) && Object.keys(m.lints).length === 1 ? undefined : m.lints,
    workspaceLints: m.workspace?.lints,
    profile: m.profile,
  });
  return pick(pb) !== pick(pa);
}

const short = (v) => (v === undefined ? 'unset' : v.length > 80 ? `${v.slice(0, 77)}...` : v);

/**
 * Cargo.toml rules beyond lints and new dependencies. Denies a member that
 * newly lacks lint inheritance or newly has a profile (a ratchet: problems the
 * manifest already had only advise); asks on test and toolchain switches and
 * major-version bumps; advises on non-caret requirements.
 */
export function manifestChecks(file, rel, before, after, proj) {
  const out = { denies: [], asks: [], advice: [] };
  const a = parseToml(after);
  if (!a) return out; // invalid TOML: cargo rejects it on its own
  const b = before === '' ? {} : parseToml(before);
  const root = a.package ? workspaceRootFor(file, a) : null;
  const opts = { ownLints: proj ? ownLintsMember(proj.config, proj.root, file) : false };
  const { fresh, legacy } = splitProblems(b ? memberProblems(b, root, opts) : [], memberProblems(a, root, opts));
  for (const p of fresh) out.denies.push(`${p.id} ${rel}: ${p.msg}.\n    fix: ${p.fix}`);
  for (const p of legacy) out.advice.push(`${p.id} ${rel} (already the case before this edit): ${p.msg}.\n    fix: ${p.fix}`);
  if (before === '' && a.package && root && !root.self) {
    out.asks.push(`${rel}: new crate \`${a.package.name ?? rel}\` in the workspace. Crate boundaries are a human decision (H-16, ARC-07).`);
  }
  if (b) {
    const wa = watchedKeys(a);
    const wb = watchedKeys(b);
    for (const label of new Set([...wb.keys(), ...wa.keys()])) {
      const x = wa.get(label);
      const y = wb.get(label);
      if (x?.value === y?.value) continue;
      if (before === '' && x && benignWhenNew(label, x)) continue;
      out.asks.push(`${rel}: \`${label}\` changes (${short(y?.value)} -> ${short(x?.value)}); ${(x ?? y).why}.`);
    }
  }
  const ra = depRequirements(a);
  const rb = b ? depRequirements(b) : new Map();
  for (const [k, req] of ra) {
    const old = rb.get(k);
    if (old === req) continue;
    const name = k.slice(k.lastIndexOf(':') + 1);
    if (old !== undefined && majorOf(old) !== null && majorOf(req) !== null && majorOf(old) !== majorOf(req)) {
      out.asks.push(`${rel}: \`${name}\` moves to a new major version ("${old}" -> "${req}"). Major-version bumps are a human decision (H-16, ARC-12).`);
    }
    const why = nonCaretRequirement(req);
    if (why) out.advice.push(`ARC-12 ${rel}: \`${name} = "${req}"\` is ${why}.\n    fix: declare a caret floor at the oldest version whose API you use (\`"1.4"\`), never \`=\`, \`~\`, \`*\` or an upper bound.`);
  }
  return out;
}

/** Dependency names declared in Cargo.toml (all dependency tables), as "kind:name". */
export function depNames(toml) {
  const m = parseToml(toml);
  return m ? dependencyNames(m) : depNamesByLine(toml);
}

/** Line-based fallback for manifests that are not valid TOML. */
function depNamesByLine(toml) {
  const names = new Set();
  let table = null;
  for (const line of toml.split(/\r?\n/)) {
    const h = line.match(/^\s*\[([^\]]+)\]\s*$/);
    if (h) {
      const t = h[1].trim();
      const inline = t.match(/(?:^|\.)((?:dev-|build-)?dependencies)\.([\w-]+)$/);
      if (inline) { names.add(`${inline[1]}:${inline[2]}`); table = null; }
      else table = /(?:^|\.)((?:dev-|build-)?dependencies)$/.test(t) ? t.match(/((?:dev-|build-)?dependencies)$/)[1] : null;
      continue;
    }
    if (table) {
      const k = line.match(/^\s*([A-Za-z0-9_-]+)\s*=/);
      if (k) names.add(`${table}:${k[1]}`);
    }
  }
  return names;
}

export function preEdit(input) {
  const tool = input.tool_name;
  const ti = input.tool_input ?? {};
  const file = ti.file_path;
  if (!file) return none();
  const proj = discoverFor(input, file);
  if (!proj) return none();
  const rel = relTo(proj.root, file);
  if (rel.startsWith('..')) return none();
  const absState = path.resolve(proj.stateDir);
  if (path.resolve(file).startsWith(absState) || /(^|\/)\.git\//.test(rel)) {
    return deny('temper: files under .git (including temper gate state and review verdicts) are not edited directly. Verdicts come from the temper:rust-reviewer subagent; gate results come from the gate command.');
  }
  if (/(^|\/)Cargo\.lock$/.test(rel)) {
    telemetry(proj, input, 'deny-lockfile', { file: rel });
    return deny(LOCKFILE_DENY(rel));
  }
  const before = readText(file) ?? '';
  const after = applyEdit(before, ti, tool);
  if (after === null) return none(); // the edit will fail on its own
  const isRust = rel.endsWith('.rs');
  const isManifest = /(^|\/)Cargo\.toml$/.test(rel);

  const denies = [];
  const asks = [];
  const advice = [];

  // Skill gate
  if ((isRust || isManifest) && proj.config.skillGate && !PRELOADED.test(input.agent_type ?? '') && !skillLoaded(proj, input)) {
    telemetry(proj, input, 'deny-skill-gate', { file: rel });
    return deny([
      'temper: the rust-craft standard is not loaded in this agent context, so this Rust edit was not applied.',
      'Invoke the Skill tool with skill "temper:rust-craft" (or read the rust-craft SKILL.md), then retry the same edit.',
      `Non-negotiables this edit will be checked against: ${IRON_RULES.join('; ')}.`,
    ].join('\n'));
  }

  // Protected configuration
  if (proj.config.protected.some((p) => globMatch(p, rel))) {
    asks.push(`${rel} is verification/agent configuration. Changing it can weaken the checks (H-05, H-12). Approve only if you asked for this change.`);
  }
  if (isManifest) {
    const mc = manifestChecks(file, rel, before, after, proj);
    denies.push(...mc.denies);
    asks.push(...mc.asks);
    advice.push(...mc.advice);
    if (lintConfigChanged(before, after)) asks.push(`${rel}: the [lints]/[profile] sections change (H-05). Approve only if intended.`);
    if (proj.config.askOnNewDependency) {
      const was = depNames(before);
      const approved = new Set(proj.config.preapprovedDependencies ?? []);
      const added = [...depNames(after)].filter((d) => !was.has(d) && !approved.has(d.split(':')[1]));
      if (added.length && before !== '') asks.push(`${rel}: new dependenc${added.length > 1 ? 'ies' : 'y'} ${added.map((d) => d.split(':')[1] + (d.startsWith('dev-') ? ' (dev)' : '')).join(', ')} (H-16, ARC-12). Approve if the addition is justified.`);
    }
  }

  if (isRust) {
    const krate = crateOf(file);
    const crateRel = krate ? relTo(krate.dir, file) : rel;
    const kind = krate ? classifyPath(crateRel, krate.hasLib) : 'prod';
    const build = isBuildScript(crateRel);
    if (tool === 'Write' && before === '' && /(^|\/)mod\.rs$/.test(rel) && kind !== 'test') {
      denies.push(`ARC-06 ${rel}: new mod.rs file.\n    fix: use \`foo.rs\` plus \`foo/bar.rs\`.`);
    }
    const b = scanSource(before, { kind, build });
    const a = scanSource(after, { kind, build });
    const exempt = (h) => (proj.config.scanner.exempt ?? []).some((e) => e.rule === h.id && (e.paths ?? []).some((p) => globMatch(p, rel)));
    const fresh = newHits(b.hits, a.hits).filter((h) => !exempt(h));
    // One approval line per exception: the #[expect] attribute carries the reason.
    const attrLints = new Set(fresh.filter((h) => h.key === 'expect-attr' && h.severity === 'ask').map((h) => h.exception));
    for (const h of fresh) {
      if (h.severity === 'ask' && h.key !== 'expect-attr' && [...attrLints].some((l) => (h.exception ?? '').length && String(l).includes(h.exception))) continue;
      if (h.severity === 'hard') denies.push(formatHit(h, rel));
      else if (h.severity === 'ask') asks.push(`Lint exception requested at ${rel}:${h.line} (${h.exception ?? h.id}): \`${h.text}\`. Approve only if the stated reason holds (H-05).`);
      else advice.push(formatHit(h, rel));
    }
    if (a.tests < b.tests) asks.push(`${rel}: this edit removes ${b.tests - a.tests} test(s) (H-12). Approve only if the test is being moved or intentionally retired.`);
    const ignored = a.ignores - b.ignores - Math.max(0, a.tests - b.tests);
    if (ignored > 0) asks.push(`${rel}: this edit adds \`#[ignore]\` to ${ignored} existing test(s) (H-12). Approve only if you asked for the test to stop running.`);
    if (a.clones > b.clones) advice.push(`H-08 ${rel}: ${a.clones - b.clones} new .clone() call(s). Each needs an ownership reason (callee stores it, cheap handle, or genuine duplication).`);
  }

  if (denies.length) {
    telemetry(proj, input, isManifest ? 'deny-manifest' : 'deny-scan', { file: rel, rules: denies.map((d) => d.split(' ')[0]) });
    return deny([
      isManifest
        ? `temper: this manifest edit breaks ${denies.length} workspace rule(s), so it was not applied. Fix the manifest text:`
        : `temper: this edit introduces ${denies.length} escape hatch(es), so it was not applied. Rewrite it without them:`,
      ...denies.slice(0, 10),
      denies.length > 10 ? `... and ${denies.length - 10} more.` : '',
      isManifest ? '' : 'If one of these is a genuine exception, put `#[expect(<lint>, reason = "...")]` on the item; the human will be asked to approve it. See rust-craft references/escape-hatches.md.',
    ].filter(Boolean).join('\n'));
  }
  if (asks.length) {
    telemetry(proj, input, 'ask', { file: rel, count: asks.length });
    return ask(`temper needs your approval:\n- ${asks.join('\n- ')}`, advice.length ? `temper advisories for this edit:\n${advice.join('\n')}` : undefined);
  }
  if (advice.length) return ctx('PreToolUse', `temper advisories for this edit (not blocking, but a reviewer will ask):\n${advice.slice(0, 8).join('\n')}`);
  return none();
}

const LOCKFILE_DENY = (rel) => `temper: ${rel} is maintained by Cargo and is never edited by hand (PROC-13). Change it with cargo: \`cargo update -p <crate> --precise <version>\` to pin or roll back one dependency, \`cargo update --workspace\` after a manifest edit, \`cargo update --dry-run\` to preview. For a merge conflict, check out one side's Cargo.lock and re-run a cargo command.`;

const MANIFEST_SHELL_DENY = 'temper: edit Cargo.toml with the Edit or Write tools, not the shell, so the manifest checks run on it (MOD-09, PERF-09, H-12, H-16). Use the Edit tool for this change.';

/**
 * Shell commands that write a file whose name ends with `target` (a regex source).
 * `copies` also treats the file as the destination of cp/mv/install.
 */
const shellWrites = (target, { copies = false } = {}) => {
  const t = `${target}(?![\\w.])`;
  const res = [
    new RegExp(`(?:&>>?|(?:^|[^>&0-9])>>?\\|?)\\s*["']?[^\\s"'|;&<>]*${t}`),
    new RegExp(`\\btee\\s+(?:-a\\s+)?["']?[^\\s"'|;&]*${t}`),
    new RegExp(`\\bsed\\b[^|;&]*\\s-[a-zA-Z]*i[^|;&]*${t}`),
    new RegExp(`\\bsed\\b[^|;&]*--in-place[^|;&]*${t}`),
    new RegExp(`\\bperl\\b[^|;&]*\\s-[a-zA-Z]*i[^|;&]*${t}`),
    new RegExp(`\\bopen\\s*\\([^)]*${target}["']\\s*,\\s*["'][wa+]`),
    new RegExp(`\\b(?:write_text|writeFileSync|Set-Content|Out-File|Add-Content)\\b[^|;&]*${t}`),
  ];
  if (copies) res.push(new RegExp(`\\b(?:cp|mv|install|Copy-Item|Move-Item)\\b[^|;&]*\\s["']?[^\\s"'|;&<>]*${t}["']?\\s*(?:$|[|;&)])`, 'm'));
  return res;
};
const SHELL_WRITE_RS = shellWrites('\\.rs');
const RS_SHELL_DENY = 'temper: write Rust sources with the Edit or Write tools, not the shell, so the standard\'s checks run on them (H-12). Use the Edit tool for this change.';

// ---- cp/mv/install/rsync/git mv/Copy-Item/Move-Item onto .rs, and git apply/am/patch (audit engine-1-8)

const statOf = (p) => { try { return fs.statSync(p); } catch { return null; } };
const isDirPath = (p) => statOf(p)?.isDirectory() ?? false;
const SPECIAL_FILE = /^\/(?:dev|proc)\//;
const MAX_SOURCE_BYTES = 4 * 1024 * 1024;
const RS_COPIES = new Set(['cp', 'mv', 'install', 'ln', 'rsync', 'scp', 'git mv', 'copy-item', 'move-item', 'cpi', 'mi', 'copy', 'move']);
const RS_MOVES = new Set(['mv', 'git mv', 'move-item', 'mi', 'move']);

/** Every .rs file (or symlink named .rs, which the compiler follows) under dir, or null past `limit` entries. */
function rustFilesUnder(dir, limit = 10000) {
  const out = [];
  const stack = [dir];
  let seen = 0;
  while (stack.length) {
    const d = stack.pop();
    let entries;
    try { entries = fs.readdirSync(d, { withFileTypes: true }); } catch { continue; }
    for (const e of entries) {
      if (++seen > limit) return null;
      const p = path.join(d, e.name);
      if (e.isDirectory()) { if (e.name !== '.git') stack.push(p); } else if ((e.isFile() || e.isSymbolicLink()) && e.name.endsWith('.rs')) out.push(p);
    }
  }
  return out;
}

/**
 * The .rs files (or the files `want` keeps) a patch command changes: `{ read: true, touched }`, or `{ read: false }` when
 * temper cannot read the patch (`later` when an earlier command in the line writes it).
 */
function patchTouches(p, c, written, want = (f) => /\.rs$/.test(f)) {
  if (p.stored) return { read: false };
  const touched = [...p.outputs, ...(p.target ? [p.target] : [])].map((w) => w.raw).filter(want);
  const inputs = p.inputs.map((w) => ({ abs: wordPath(w, p.cwd) }));
  if (!inputs.length) {
    if (c.stdin?.kind === 'heredoc' && c.stdin.literal !== null && !c.piped) inputs.push({ body: c.stdin.literal });
    else if (c.stdin?.kind === 'file' && !c.piped) inputs.push({ abs: wordPath(c.stdin.word, c.cwd) });
    else return { read: false };
  }
  for (const i of inputs) {
    if (i.abs && written.has(i.abs)) return { read: false, later: true };
    const st = i.abs && !SPECIAL_FILE.test(norm(i.abs)) ? statOf(i.abs) : null;
    const text = i.body ?? (st?.isFile() && st.size <= MAX_SOURCE_BYTES * 8 ? readText(i.abs) : null);
    if (text === null) return { read: false };
    touched.push(...patchFiles(text).filter(want));
  }
  return { read: true, touched: [...new Set(touched.map((f) => f.replace(/^[ab]\//, '')))] };
}

const RS_COPY_SCANNED = Symbol('temper-rs-copies-scanned');

/**
 * cp/mv/install/rsync/git mv/Copy-Item/Move-Item onto .rs files, and git apply/am/patch of
 * Rust sources (audit engine-1-8), read with scripts/lib/shell.mjs. A copy whose source
 * temper can read is scanned the way preEdit scans a Write of that content to the
 * destination; a move of an in-repository .rs file compares against the source's own
 * content, as the gate does for a rename, so moving legacy debt does not count as adding
 * it. A copy duplicates the content, so it is compared with the destination only, as a
 * Write and the gate's view of a new file are. A copied tree that carries its crate's
 * Cargo.toml keeps the source crate's layout (a test stays a test in the new crate). A
 * write temper cannot read (variable, glob, stdin, missing source) is refused, and so is a
 * patch that touches .rs files: the Edit tool applies it with the checks.
 * Returns a hook result, or null when the command writes no Rust source.
 */
function shellRustWrites(cmd, proj, input) {
  const denies = [];
  const asks = [];
  const advice = [];
  const unknown = []; // { t, why: 'dyn' | 'cwd' | 'patch' | 'later' | 'stale' }
  const patched = [];
  const direct = []; // content written straight into a .rs file (`> x.r\s`, `curl -o x.rs`)
  const inProject = (abs) => {
    const r = relTo(proj.root, abs);
    return !(r.startsWith('..') || path.isAbsolute(r) || /^[A-Za-z]:/.test(r) || /(^|\/)\.git(\/|$)/.test(r));
  };
  // The gate's scratch directories (project.mjs EXCLUDE): nothing there is gated or built.
  const scratch = (abs) => /^(?:\.temper|target)(?:\/|$)/.test(relTo(proj.root, abs));
  const exempt = (h, rel) => (proj.config.scanner.exempt ?? []).some((e) => e.rule === h.id && (e.paths ?? []).some((p) => globMatch(p, rel)));
  const kindOf = (abs) => {
    const krate = crateOf(abs);
    const crateRel = krate ? relTo(krate.dir, abs) : relTo(proj.root, abs);
    return { kind: krate ? classifyPath(crateRel, krate.hasLib) : 'prod', build: isBuildScript(crateRel) };
  };
  // A directory a copy would put Rust sources in: one that holds .rs files, or a crate's src/ (or under it), tests/, benches/ or examples/.
  const rustDir = (abs) => {
    if (!inProject(abs) || scratch(abs)) return false;
    try { if (fs.readdirSync(abs).some((n) => n.endsWith('.rs'))) return true; } catch { /* a new directory */ }
    const krate = crateOf(path.join(abs, 'x.rs'));
    return Boolean(krate) && /^(?:src(?:\/.*)?|tests|benches|examples)$/.test(relTo(krate.dir, abs));
  };

  // `tree` is the copied directory ({ real, root }) when `from` came with one.
  const checkFile = (target, from, move, tree = null) => {
    if (path.resolve(target) === path.resolve(from)) return;
    if (scratch(target)) return;
    const st = SPECIAL_FILE.test(norm(from)) ? null : statOf(from);
    const after = st?.isFile() && st.size <= MAX_SOURCE_BYTES ? readText(from) : null;
    if (after === null) { unknown.push({ t: norm(from), why: 'dyn' }); return; }
    const rel = relTo(proj.root, target);
    const exists = statOf(target)?.isFile() ?? false;
    const before = exists ? readText(target) ?? '' : '';
    // A tree that carries its crate's manifest lands as that crate: the target maps the way its source does.
    const srcCrate = tree ? crateOf(from) : null;
    const carried = srcCrate && !path.relative(tree.real, srcCrate.dir).startsWith('..') && !path.isAbsolute(path.relative(tree.real, srcCrate.dir));
    const { kind, build } = carried ? kindOf(from) : kindOf(target);
    // A moved repository source brings its own findings along: count them as already there,
    // unless the move changes how the scanner reads it (test or example code into src/).
    const fromKind = move && from.endsWith('.rs') && inProject(from) ? kindOf(from) : null;
    const lenient = Boolean(fromKind) && fromKind.kind === kind && fromKind.build === build;
    if (proj.config.protected.some((p) => globMatch(p, rel))) asks.push(`${rel} is verification/agent configuration. Changing it can weaken the checks (H-05, H-12). Approve only if you asked for this change.`);
    if (!exists && /(^|\/)mod\.rs$/.test(rel) && kind !== 'test' && !(lenient && path.basename(from) === 'mod.rs')) {
      denies.push(`ARC-06 ${rel}: new mod.rs file.\n    fix: use \`foo.rs\` plus \`foo/bar.rs\`.`);
    }
    if (lenient && !exists) return; // nothing new can appear
    const b = scanSource(before, { kind, build });
    const a = scanSource(after, { kind, build });
    const was = lenient
      ? { hits: [...b.hits, ...a.hits], tests: b.tests + a.tests, ignores: b.ignores + a.ignores, clones: b.clones + a.clones }
      : b;
    const fresh = newHits(was.hits, a.hits).filter((h) => !exempt(h, rel));
    const attrLints = new Set(fresh.filter((h) => h.key === 'expect-attr' && h.severity === 'ask').map((h) => h.exception));
    for (const h of fresh) {
      if (h.severity === 'ask' && h.key !== 'expect-attr' && [...attrLints].some((l) => (h.exception ?? '').length && String(l).includes(h.exception))) continue;
      if (h.severity === 'hard') denies.push(formatHit(h, rel));
      else if (h.severity === 'ask') asks.push(`Lint exception requested at ${rel}:${h.line} (${h.exception ?? h.id}): \`${h.text}\`. Approve only if the stated reason holds (H-05).`);
      else advice.push(formatHit(h, rel));
    }
    if (a.tests < was.tests) asks.push(`${rel}: this command removes ${was.tests - a.tests} test(s) (H-12). Approve only if the test is being moved or intentionally retired.`);
    const ignored = a.ignores - was.ignores - Math.max(0, a.tests - was.tests);
    if (ignored > 0) asks.push(`${rel}: this command adds \`#[ignore]\` to ${ignored} existing test(s) (H-12). Approve only if you asked for the test to stop running.`);
    if (a.clones > was.clones) advice.push(`H-08 ${rel}: ${a.clones - was.clones} new .clone() call(s). Each needs an ownership reason (callee stores it, cheap handle, or genuine duplication).`);
  };

  // Paths an earlier cp/mv in this command creates: { real, copy, step, stale }, where `real` is
  // what they hold, `copy` says the content was duplicated (so moving it on is still a copy),
  // `step` is the command that made them and `stale` says `real` was rewritten before that.
  const earlierCopies = new Map();
  const earlier = (abs) => {
    for (let p = abs, rest = ''; ; rest = path.join(path.basename(p), rest), p = path.dirname(p)) {
      const hit = earlierCopies.get(p);
      if (hit) return { real: rest ? path.join(hit.real, rest) : hit.real, copy: hit.copy, step: hit.step, stale: hit.stale };
      if (path.dirname(p) === p) return null;
    }
  };
  // Every path a command in this line writes, with the last command that wrote it. A file that a
  // write other than a copy (a redirect, tee, sed -i, curl -o, interpreter code) changed after
  // the copy that made it, or that no copy made, holds content the disk does not show yet.
  let step = 0;
  const lastWrite = new Map();
  const rewritten = (abs, hit) => {
    if (hit?.stale) return true;
    const since = hit?.step ?? 0;
    abs = path.resolve(abs);
    const inside = `${abs}${path.sep}`;
    for (const [w, n] of lastWrite) {
      if (n <= since) continue;
      if (w === abs || w.startsWith(inside) || abs.startsWith(`${w}${path.sep}`)) return true;
    }
    return false;
  };
  // What `abs` holds when this command reaches it: what an earlier copy put there (over a file
  // that already exists too), else what is on disk now (a file a copied tree did not bring).
  const prior = (abs) => {
    const hit = earlier(abs);
    return hit && (!statOf(abs) || statOf(hit.real)) ? hit : null;
  };
  // A readable source copied to a destination held in a variable (`"$BACKUP"`, `${OUT_DIR}/`)
  // lands clean wherever it goes when it scans clean as production code and does not land as a
  // mod.rs. A literal destination after a `cd` temper cannot follow is still refused.
  const cleanAnywhere = (srcAbs, s, t) => {
    const made = prior(srcAbs);
    if (rewritten(srcAbs, made)) return false;
    const real = made?.real ?? srcAbs;
    const st = SPECIAL_FILE.test(norm(real)) ? null : statOf(real);
    const text = st?.isFile() && st.size <= MAX_SOURCE_BYTES ? readText(real) : null;
    if (text === null) return false;
    const name = /[\\/]$/.test(t.raw) || t.intoDir ? path.basename(real) : path.basename(t.raw);
    if (name === 'mod.rs' || (path.basename(real) === 'mod.rs' && !name.endsWith('.rs'))) return false;
    return !scanSource(text, { kind: 'prod', build: false }).hits.some((h) => h.severity === 'hard' || h.severity === 'ask');
  };
  // `ln -s` reads a relative source from the link's directory (`ln -sr` from the current one).
  const linkSource = (t, s, relative) => {
    if (relative) return wordPath(s, t.cwd);
    if (s.value === null || s.glob) return null;
    const abs = rsResolvePath(s.value, null);
    if (abs) return abs;
    const at = landing(t, s, isDirPath(t.path));
    return at ? path.resolve(path.dirname(at), s.value) : null;
  };
  const copy = (t, move, symbolic = false, relative = false) => {
    const destAbs = t.path;
    for (const s of t.sources) {
      const srcAbs = symbolic ? linkSource(t, s, relative) : wordPath(s, t.cwd);
      if (srcAbs !== null && t.value === null && cleanAnywhere(srcAbs, s, t)) continue;
      if (srcAbs === null || destAbs === null) {
        const why = t.cwd === null && s.value !== null && !s.glob && t.value !== null && !t.glob ? 'cwd' : 'dyn';
        const named = /\.rs$/.test(s.raw) || /\.rs$/.test(t.raw);
        // A source temper cannot name, copied into a Rust source directory, may be Rust (`cp $DIR/* src/`).
        const intoRust = destAbs !== null && srcAbs === null && !/\.(?!rs$)\w+$/.test(s.raw)
          && (t.intoDir || isDirPath(destAbs) || /[\\/]$/.test(t.raw) || (t.recursive && !/\.\w+$/.test(path.basename(destAbs))))
          && rustDir(destAbs);
        if ((named && (destAbs === null || (inProject(destAbs) && !scratch(destAbs)))) || intoRust) unknown.push({ t: srcAbs === null ? s.raw : t.raw, why });
        continue;
      }
      // A path an earlier command in this line creates reads as what it was made from.
      const made = prior(srcAbs);
      const real = made?.real ?? srcAbs;
      const copied = !move || Boolean(made?.copy);
      const stale = rewritten(srcAbs, made);
      const st = SPECIAL_FILE.test(norm(real)) ? null : statOf(real);
      const staleUnread = () => unknown.push({ t: s.raw, why: 'stale' });
      if (st?.isDirectory()) {
        // A symbolic link to a directory brings its whole tree, as a recursive copy does.
        if (!t.recursive && !symbolic) continue;
        // rsync copies `src/`'s contents, and puts `src` itself inside the destination.
        const contents = t.verb === 'rsync' && /[\\/]$/.test(s.raw);
        const root = contents ? destAbs : (t.verb === 'rsync' || (t.intoDir ?? isDirPath(destAbs))) ? path.join(destAbs, path.basename(srcAbs)) : destAbs;
        earlierCopies.set(root, { real, copy: copied, step, stale });
        if (!inProject(root) || scratch(root)) continue;
        if (stale) { staleUnread(); continue; }
        const files = rustFilesUnder(real);
        if (files === null) { unknown.push({ t: s.raw, why: 'dyn' }); continue; }
        for (const f of files) {
          const target = path.join(root, path.relative(real, f));
          if (inProject(target)) checkFile(target, f, !copied, { real, root });
        }
        continue;
      }
      const target = landing(t, s, isDirPath(destAbs));
      if (st?.isFile()) earlierCopies.set(target, { real, copy: copied, step, stale });
      if (!target.endsWith('.rs') || !inProject(target) || scratch(target)) continue;
      if (stale) { staleUnread(); continue; }
      if (!st?.isFile()) { unknown.push({ t: s.raw, why: 'dyn' }); continue; }
      checkFile(target, real, !copied);
    }
  };

  const written = new Set();
  for (const c of parseCommands(cmd, { cwd: input.cwd ?? proj.root, top: proj.gitRoot ?? proj.root })) {
    const writes = commandWrites(c);
    for (const w of writes) if (w.verb === 'redirect' && w.path) written.add(w.path);
    const p = patchCommand(c);
    if (p?.apply) {
      const r = patchTouches(p, c, written);
      if (!r.read) unknown.push({ t: [c.argv0, ...c.args].map((x) => x.raw).join(' '), why: r.later ? 'later' : 'patch' });
      else patched.push(...r.touched);
    }
    const lnFlag = (letter, long) => c.name === 'ln' && c.args.some((a) => new RegExp(`^-[a-zA-Z]*${letter}`).test(a.raw) || a.raw === long);
    const symbolic = lnFlag('s', '--symbolic');
    const relative = symbolic && lnFlag('r', '--relative');
    step += 1;
    for (const t of writes) if (RS_COPIES.has(t.verb)) copy(t, RS_MOVES.has(t.verb), t.verb === 'ln' && symbolic, t.verb === 'ln' && relative);
    for (const w of writes) if (w.path) { written.add(w.path); lastWrite.set(path.resolve(w.path), step); }
    // What SHELL_WRITE_RS reads from the raw text, read the way the shell does: a name spelled
    // `x.r\s` or `x.r''s`, or a writer the regex does not know (curl -o, dd of=, sponge).
    for (const w of writes) {
      if (RS_COPIES.has(w.verb) || w.verb.startsWith('git ') || w.dir || w.glob || !w.path) continue;
      if (w.path.endsWith('.rs') && inProject(w.path) && !scratch(w.path)) direct.push(w.path);
    }
  }

  if (direct.length) {
    telemetry(proj, input, 'deny-shell-write', { via: 'shell-reader' });
    return deny(RS_SHELL_DENY);
  }
  const unreadPatch = unknown.find((u) => u.why === 'patch' || u.why === 'later');
  if (unreadPatch) {
    telemetry(proj, input, 'deny-shell-write', { via: 'patch-unread' });
    const why = unreadPatch.why === 'later'
      ? 'its patch file does not exist yet, or is rewritten, earlier in this command'
      : 'stdin, a variable or a missing file';
    return deny(`temper: temper cannot read the patch that \`${unreadPatch.t.slice(0, 120)}\` applies (${why}), so it cannot tell whether it changes Rust sources, and the command was not run (H-12). Save the patch to a file in one command and check it with \`git apply --stat <file>\`; make changes to .rs files with the Edit tool so the standard's checks run on them.`);
  }
  if (unknown.length) {
    telemetry(proj, input, 'deny-shell-write', { via: 'copy-unread' });
    const u = unknown[0];
    const why = u.why === 'cwd'
      ? `temper cannot tell which directory \`${u.t.slice(0, 120)}\` is relative to (an earlier \`cd\` it cannot follow)`
      : u.why === 'stale'
        ? `temper cannot read what this command would write to a .rs file (\`${u.t.slice(0, 120)}\` is rewritten earlier in this command, so the file on disk is not what gets copied)`
        : `temper cannot read what this command would write to a .rs file (\`${u.t.slice(0, 120)}\` is a variable, glob, stdin or a missing path)`;
    return deny(`temper: write Rust sources with the Edit or Write tools, not the shell, so the standard's checks run on them (H-12). ${why}, so it was not run. Copy from a literal path that exists, or use the Edit tool.`);
  }
  if (patched.length) {
    telemetry(proj, input, 'deny-shell-write', { via: 'patch', files: patched.length });
    const list = patched.slice(0, 5).join(', ') + (patched.length > 5 ? `, and ${patched.length - 5} more` : '');
    return deny(`temper: this patch changes Rust sources (${list}), and applying it from the shell skips the standard's checks (H-12), so it was not run. Make the change with the Edit tool, so the scanner checks each hunk. \`git apply --check\`, \`git apply --stat\` and \`patch --dry-run\` still run for inspecting it.`);
  }
  if (denies.length) {
    telemetry(proj, input, 'deny-scan', { via: 'shell-copy', rules: denies.map((d) => d.split(' ')[0]) });
    return deny([
      `temper: this command writes ${denies.length} escape hatch(es) into Rust sources, so it was not run. Rewrite the source without them, or make the change with the Edit or Write tools:`,
      ...denies.slice(0, 10),
      denies.length > 10 ? `... and ${denies.length - 10} more.` : '',
      'If one of these is a genuine exception, put `#[expect(<lint>, reason = "...")]` on the item; the human will be asked to approve it. See rust-craft references/escape-hatches.md.',
    ].filter(Boolean).join('\n'));
  }
  if (asks.length) {
    telemetry(proj, input, 'ask', { via: 'shell-copy', count: asks.length });
    return ask(`temper needs your approval:\n- ${asks.join('\n- ')}`, advice.length ? `temper advisories for this command:\n${advice.join('\n')}` : undefined);
  }
  if (advice.length) return ctx('PreToolUse', `temper advisories for this command (not blocking, but a reviewer will ask):\n${advice.slice(0, 8).join('\n')}`);
  return null;
}
const SHELL_WRITE_LOCK = shellWrites('Cargo\\.lock', { copies: true });
const SHELL_WRITE_TOML = shellWrites('Cargo\\.toml', { copies: true });
const SHELL_WRITE_BASELINE = shellWrites('temper-baseline\\.json', { copies: true });
const SHELL_WRITE_VENDORED = shellWrites('\\.claude/temper/[\\w./-]*', { copies: true });
const VENDORED_SHELL_DENY = 'temper: .claude/temper/ is the vendored temper CLI that CI and the pre-commit hook run (the lint ratchet and the baseline-raise check live in it). It is written only by `temper setup`, never by hand (H-12). Leave it unchanged, or ask the human to re-vendor it.';
const STATE_SHELL_DENY = 'temper: temper\'s state dir (.git/temper/, or target/temper/ without git) holds the gate results and review verdicts, and the shell does not write into it: verdicts come only from the temper:rust-reviewer subagent; gate results come only from the gate command (`node "$TEMPER_CLI" gate`). Reading the files, copying them elsewhere and deleting telemetry.jsonl stay allowed.';
// `$VAR/temper/<entry>` counts as the state dir unless VAR is a home or temp directory.
const OUTSIDE_VARS = /^(?:HOME|TMPDIR|TMP|TEMP|USERPROFILE|APPDATA|LOCALAPPDATA|PWD|OLDPWD|XDG_\w+)$/;

/**
 * Shell writes into the project's temper state dir: `.git/temper/` under any prefix, the
 * absolute `proj.stateDir` (in a worktree it is in the main repository's git dir) with
 * either separator, `target/temper/` when there is no git, and `$VAR/temper/<state entry>`.
 * Only destinations count, as scripts/lib/shell.mjs reads them: redirection targets, the
 * destination of a copy (and where each source lands in it), tee, sed -i, dd, curl, wget,
 * tar -x -C, unzip -d, the PowerShell writers and interpreter code, inside `bash -c` and
 * compound commands too. Variables, `$(git rev-parse --git-common-dir)` and `cd`/`pushd`
 * are followed. Reading a state file, or redirecting what a read prints elsewhere, does
 * not match, and neither does quoted text.
 */
export function stateDirWrite(proj, cmd, cwd = proj.root) {
  const win = process.platform === 'win32';
  const fold = (s) => (win ? s.toLowerCase() : s);
  const slash = (s) => String(s).replace(/\\/g, '/');
  const absState = fold(slash(path.resolve(proj.stateDir)));
  const start = path.resolve(cwd);
  // shell.mjs keeps a backslash before a word character as a separator when it takes the
  // command for PowerShell, but a bash function can carry a cmdlet-shaped name and bash then
  // drops it (`copy() { cat; }; ... | copy > .git/tem\per/...`). Deny if either reading is the state dir.
  const bashReading = (s) => (s == null ? s : String(s).replace(/\\(?=[\w-])/g, ''));
  const isState = (text, value, base) => isStateAs(text, value, base)
    || (/\\[\w-]/.test(`${text}${value ?? ''}`) && isStateAs(bashReading(text), bashReading(value), base));
  const isStateAs = (text, value, base) => {
    const v =/^\$\{?(\w+)\}?\/temper\/(?:(?:verdicts|gate|logs|sessions)(?:\/|$)|telemetry\.jsonl$|findings-)/.exec(slash(text));
    if (v && !OUTSIDE_VARS.test(v[1])) return true;
    let p = slash(msysPath(value ?? text));
    if (!/^(?:\/|[a-z]:\/|\$|~)/i.test(p)) p = `${slash(base)}/${p}`;
    const f = fold(path.posix.normalize(p));
    if (/(?:^|\/)\.git\/temper(?:\/|$)/.test(f)) return true;
    if (!proj.gitRoot && /(?:^|\/)target\/temper(?:\/|$)/.test(f)) return true;
    return value !== null && (f === absState || f.startsWith(`${absState}/`));
  };
  const gitDir = proj.gitRoot ? path.dirname(path.resolve(proj.stateDir)) : '.git';
  for (const t of writeTargets(cmd, { cwd: start, top: proj.gitRoot ?? proj.root, gitDir })) {
    const base = t.cwd ?? start;
    const value = t.glob ? null : t.value;
    if (isState(t.text, value, base)) return true;
    // A copy into a directory lands as <dest>/<source name> (`mv temper .git/`).
    for (const s of t.sources ?? []) {
      const name = s.value === null ? null : path.posix.basename(slash(s.value).replace(/\/+$/, ''));
      if (name && isState(`${t.text}/${name}`, value === null ? null : `${value}/${name}`, base)) return true;
    }
  }
  return false;
}

const BASELINE_SHELL_DENY = 'temper: .claude/temper-baseline.json records the accepted lint debt and is written only by `temper baseline` (H-12). After fixing findings, lower it with `node "$TEMPER_CLI" baseline --tighten`. Raising it (`baseline --update`) is a human decision: ask the human.';

/** The project that owns `abs` (a nested git checkout, such as a worktree under .claude/worktrees/, owns its files), as preEdit finds it. */
function ownerOf(proj, rel, cache) {
  const parts = rel.split('/');
  for (let k = parts.length - 1; k > 0; k--) {
    const dir = path.join(proj.root, ...parts.slice(0, k));
    if (!cache.has(dir)) cache.set(dir, fs.existsSync(path.join(dir, '.git')) ? discover(dir) ?? proj : null);
    const owner = cache.get(dir);
    if (owner) return owner;
  }
  return proj;
}

/** Files under `dir`, relative to it with `/`, or null past `limit` entries (a copied tree's protected files land under its destination). */
function configFilesUnder(dir, limit = 10000) {
  const out = [];
  const stack = [''];
  let seen = 0;
  while (stack.length) {
    const rel = stack.pop();
    let entries;
    try { entries = fs.readdirSync(path.join(dir, rel), { withFileTypes: true }); } catch { continue; }
    for (const e of entries) {
      if (++seen > limit) return null;
      const r = rel ? `${rel}/${e.name}` : e.name;
      if (e.isDirectory()) { if (e.name !== '.git') stack.push(r); } else out.push(r);
    }
  }
  return out;
}

/** Protected globs a whole directory `rel` replaces: `<dir>/**` at or below it, and every glob for the project root. */
const protectedUnder = (globs, rel) => (rel === '' ? globs : globs.filter((g) => g.endsWith('/**') && (globMatch(g.slice(0, -3), rel) || g.startsWith(`${rel}/`))));

/**
 * The files a patch command changes (its -o output, its target and the files the patch names):
 * `{ read: true, touched }`, or `{ read: false }` when temper cannot read the patch.
 */
function configPatchTouches(p, c) {
  if (p.stored) return { read: false };
  const real = (f) => f !== '/dev/null';
  const touched = [...p.outputs, ...(p.target ? [p.target] : [])].map((w) => w.raw).filter(real);
  const wordAbs = (w, cwd) => (w && !w.glob ? resolvePath(w.value, cwd) : null);
  const inputs = p.inputs.map((w) => ({ abs: wordAbs(w, p.cwd) }));
  if (!inputs.length) {
    if (c.stdin?.kind === 'heredoc' && c.stdin.literal !== null && !c.piped) inputs.push({ body: c.stdin.literal });
    else if (c.stdin?.kind === 'file' && !c.piped) inputs.push({ abs: wordAbs(c.stdin.word, c.cwd) });
    else return { read: false };
  }
  for (const i of inputs) {
    let st = null;
    if (i.abs && !/^\/(?:dev|proc)\//.test(norm(i.abs))) { try { st = fs.statSync(i.abs); } catch { st = null; } }
    const text = i.body ?? (st?.isFile() && st.size <= 32 * 1024 * 1024 ? readText(i.abs) : null);
    if (text === null) return { read: false };
    touched.push(...protectedPatchFiles(text).filter(real));
  }
  return { read: true, touched: [...new Set(touched.map((f) => f.replace(/^[ab]\//, '')))] };
}

/**
 * Project-relative paths of protected configuration (config.protected) that `cmd` writes,
 * read with scripts/lib/shell.mjs. Relative names resolve against the directory each command
 * runs in; after a `cd` temper cannot follow, against both the starting directory and the
 * project root. A whole directory written in one go (`cp -r d .cargo`, `mv d .github`,
 * `git checkout <rev> -- .github`, `git restore --source=<rev> .`) reports the protected
 * globs it replaces. `git checkout`/`git restore` from HEAD or the index pass; for a
 * wildcard pathspec from another revision, git lists the files it would change. A patch
 * (git apply, git am, patch) writes the files it names. Returns `{ files, unreadPatch }`:
 * `unreadPatch` is the first patch command whose patch temper cannot read (a pipe, a variable,
 * a missing file, `git am --continue`), or null.
 */
function protectedShellWrites(proj, cmd, cwd) {
  const start = cwd && fs.existsSync(cwd) ? path.resolve(cwd) : proj.root;
  const rels = [];
  let unreadPatch = null;
  const owners = new Map();
  const stat = (p) => { try { return fs.statSync(p); } catch { return null; } };
  const check = (abs, tree) => {
    const rel = relTo(proj.root, abs);
    if (rel.startsWith('..') || path.isAbsolute(rel) || (!rel && !tree)) return;
    const owner = rel ? ownerOf(proj, rel, owners) : proj;
    const own = owner === proj ? rel : relTo(owner.root, abs);
    const hits = own && owner.config.protected.some((p) => globMatch(p, own)) ? [rel] : [];
    if (tree) hits.push(...protectedUnder(owner.config.protected, own).map((g) => (owner === proj ? g : `${relTo(proj.root, owner.root)}/${g}`)));
    for (const h of hits) if (!rels.includes(h)) rels.push(h);
  };
  // A wildcard or magic pathspec (`git checkout main -- '*.toml'`), or a directory: the files it would change, as git lists them.
  const pathspec = (t, base, fallback = true) => {
    const top = run('git', ['rev-parse', '--show-toplevel'], { cwd: base, timeoutMs: 5000 });
    const diff = run('git', ['diff', '--name-only', '--no-renames', '-z', t.source, '--', t.raw], { cwd: base, timeoutMs: 5000 });
    if (top.error || diff.error) { if (fallback) check(proj.root, true); return; } // git could not say: every protected glob
    if (top.code !== 0 || diff.code !== 0) return; // not a repository, or no such revision: the restore fails too
    for (const f of diff.stdout.split('\0').filter(Boolean)) check(path.resolve(top.stdout.trim(), f), false);
  };
  for (const c of protectedCommands(cmd, { cwd: start, top: proj.gitRoot ?? proj.root })) {
    // A patch writes the files it names, its target and its -o output. One temper cannot read asks.
    const p = protectedPatch(c);
    if (p?.apply) {
      const r = configPatchTouches(p, c);
      if (!r.read && unreadPatch === null) unreadPatch = [c.argv0, ...c.args].map((w) => w.raw).join(' ');
      const named = [...p.outputs, ...(p.target ? [p.target] : [])].map((w) => w.value);
      const listed = r.read ? r.touched.filter((f) => !named.includes(f)) : [];
      for (const base of p.cwd ? [p.cwd] : [start, proj.root]) {
        for (const v of named) { const abs = resolvePath(v, base); if (abs) check(abs, false); }
        for (const f of listed) for (const b of p.tool === 'patch' ? [base] : [base, proj.root]) check(path.resolve(b, f), false);
      }
    }
    for (const t of protectedWrites(c)) {
      const restore = t.verb === 'git checkout' || t.verb === 'git restore';
      if (restore && (t.source === null || t.source === 'HEAD' || t.source === '@')) continue; // undoing one's own edit
      for (const base of t.cwd ? [t.cwd] : [start, proj.root]) {
        if (restore && pathspecPattern(t)) { pathspec(t, base); continue; }
        const dest = t.glob ? null : resolvePath(t.value, base);
        if (!dest) continue;
        if (!t.sources) {
          check(dest, restore);
          // A directory from another revision brings back the protected files git lists in it (`docs/clippy.toml`).
          if (restore && !(stat(dest)?.isFile() ?? false)) pathspec(t, base, false);
          continue;
        }
        const destIsDir = stat(dest)?.isDirectory() ?? false;
        for (const s of t.sources) {
          const land = protectedLanding({ ...t, path: dest }, s, destIsDir);
          if (!land) continue;
          // A recursive copy or a move of anything but a known file may bring a whole tree,
          // and with it the protected files inside it (`cfg/sub/clippy.toml`).
          const srcAbs = resolvePath(s.value, base);
          const srcStat = srcAbs ? stat(srcAbs) : null;
          check(land, t.recursive && !(srcStat?.isFile() ?? false));
          if (t.recursive && srcStat?.isDirectory()) for (const f of configFilesUnder(srcAbs) ?? []) check(path.join(land, f), false);
        }
      }
    }
  }
  return { files: rels, unreadPatch };
}

/** Shell segments of a command with quotes removed (`"--update"` and `'x'` read as `--update` and `x`). */
const unquotedSegments = (cmd) => String(cmd).split(/&&|\|\||[;|\n]/)
  .map((seg) => seg.replace(/"((?:[^"\\]|\\.)*)"|'([^']*)'/g, (_m, a, b) => a ?? b ?? ''));

/**
 * `temper baseline --init` / `--update` through the plugin CLI, the vendored copy
 * (`node .claude/temper/temper.mjs`), `$TEMPER_CLI`, or any variable holding the
 * CLI path (`node $T baseline --update`). They record the current findings as
 * accepted debt, so they weaken verification (H-12). Quotes do not hide a flag.
 */
export function baselineRaise(cmd) {
  for (const seg of unquotedSegments(cmd)) {
    const call = /(?:^|[\s=])(?:\S*temper\.mjs|\$\{?\w+\}?|temper)\s+(?:--?\S+\s+)*baseline\b(.*)$/.exec(seg);
    if (!call) continue;
    const flag = /(?:^|\s)--?(init|update)(?![\w-])/.exec(call[1]);
    if (flag) return `--${flag[1]}`;
  }
  return null;
}

/**
 * git commands that put an older `.claude/` (the baseline, settings, the vendored
 * CLI) back into the working tree: checkout/restore/stash naming a `.claude`
 * path, and `git show ... >` redirected into `.claude/`. Restoring from HEAD or
 * the index (`git checkout -- f`, `git restore f`) undoes one's own edit and passes.
 */
export function claudeRestore(cmd) {
  for (const seg of unquotedSegments(cmd)) {
    const m = /\bgit\s+(?:(?:-C|-c)\s+\S+\s+)*(checkout|restore|stash)\b(.*)$/.exec(seg);
    if (m && /(?:^|[\s=:/])\.claude(?:\/|\s|$)|temper-baseline\.json/.test(m[2])) {
      const args = m[2].trim().split(/\s+/).filter(Boolean);
      let source = null;
      if (m[1] === 'checkout') {
        const dd = args.indexOf('--');
        const before = (dd === -1 ? args.slice(0, -1) : args.slice(0, dd)).filter((a) => !a.startsWith('-'));
        source = before[0] ?? null; // no tree-ish: the index
      } else if (m[1] === 'restore') {
        const s = args.findIndex((a) => a === '-s' || a === '--source' || a.startsWith('--source=') || /^-s./.test(a));
        if (s !== -1) source = args[s].includes('=') ? args[s].slice(args[s].indexOf('=') + 1) : /^-s./.test(args[s]) ? args[s].slice(2) : args[s + 1];
      } else source = 'stash';
      if (source !== null && source !== 'HEAD' && source !== '@') return seg.trim();
    }
    if (/\bgit\s+(?:-C\s+\S+\s+)*show\b[^>]*>\s*\S*(?:^|\/|\s)?\.claude\//.test(seg)) return seg.trim();
  }
  return null;
}

/** Command segments with quoted strings blanked, so flags inside messages do not count. */
const segments = (cmd) => cmd.replace(/'[^']*'|"(?:[^"\\]|\\.)*"/g, "''").split(/&&|\|\||[;|\n]/);
const isCargo = (seg) => /(?:^|\s)cargo(?:\s|$)/.test(seg);

// Prose blanking for the --no-verify and lint-lowering checks (engine-1-17). Those checks
// match the raw text, so a command handed to `sh -c`, `rebase -x` or `powershell -Command`
// is still read; only text that is clearly prose is blanked to '' first.
const QUOTED = `'[^']*'|"(?:[^"\\\\]|\\\\.)*"`;
const HEREDOC = /(?<!<)<<-?[ \t]*(['"]?)([\w.-]+)\1([^\n]*)\n[\s\S]*?\n\t*\2(?=\n|$)/g;
const FEEDS_SHELL = /\||(?:^|[\s(])["']?(?:\S*[\\/])?(?:(?:ba|da|z|k|pw)?sh|powershell|cmd|eval|source|xargs|python[\d.]*|py|node|perl|ruby)(?:\.exe)?["']?(?=\s|$)/i;
const MESSAGE_VALUE = new RegExp(`((?:^|\\s)(?:--(?:message|body|title|notes|description)=?|-[a-zA-Z]*m|-[bt])\\s*)(?:${QUOTED}|(?!--)[^\\s'"]+)`, 'g');
// Command substitution runs, except a bare `$(cat <<'EOF' ... EOF)` whose body is already blanked.
const CAT_HEREDOC = /\$\(\s*cat\s+<<-?(['"]?)([\w.-]+)\1[ \t]*\n\2\s*\)/g;
const runsCode = (text) => /\$\(|\x60/.test(text.replace(CAT_HEREDOC, ''));
const SEARCH = new RegExp(`(?:^|[;&|(\\x60\\n])\\s*(?:\\w+=\\S*\\s+|(?:sudo|env|time|xargs)\\s+)*(?:\\S*[\\\\/])?(?:git\\s+grep|[ef]?grep|rg|ag|ack)(?:\\.exe)?(?=\\s|$)(?:${QUOTED}|[^'"|;&\\n])*`, 'g');

/** `cmd` with here-document bodies, message option values and search patterns blanked. */
function blankProse(cmd) {
  return cmd
    // A backslash-newline continues the line.
    .replace(/\\\r?\n/g, ' ')
    // A here-document body is text, unless a shell reads it or an unquoted body runs `$(...)`.
    .replace(HEREDOC, (m, q, tag, rest, off, s) => {
      const head = s.slice(s.lastIndexOf('\n', off) + 1, off).split(/&&|\|\||;|\(/).pop() + rest;
      return FEEDS_SHELL.test(head) || (!q && /\$\(|`/.test(m)) ? m : `<<${q}${tag}${q}${rest}\n${tag}`;
    })
    // Command substitution runs, so a value or pattern that holds one is never blanked.
    .replace(MESSAGE_VALUE, (m, pre) => (runsCode(m) ? m : `${pre}''`))
    .replace(SEARCH, (m) => (runsCode(m) ? m : m.replace(new RegExp(QUOTED, 'g'), "''").replace(/\s--\s.*/, ' -- ')));
}

/** `--no-verify` anywhere, or `-n` in a `git commit` short-flag cluster before any valued flag. */
// Each option word matches one way only, so a long option list cannot backtrack exponentially.
const GIT_COMMIT_N = new RegExp(`\\bgit\\s+(?:-[cC]\\s+(?:${QUOTED}|\\S+)\\s+|--?(?![cC]\\s)\\w[\\w-]*(?:=(?:${QUOTED}|\\S*))?\\s+)*commit(?=\\s|$)(?:(?!\\s--\\s)[^|;&\\n])*\\s["']?-[aehiopqsvz]*n`);
const skipsGitHooks = (text) => /--no-verify\b/.test(text) || GIT_COMMIT_N.test(text);

/** `cargo clippy`/`rustc` with -A, --allow or --cap-lints, or a *RUSTFLAGS assignment holding one. */
const lowersLints = (text) => text.split(/[|;&\n]/).some((seg) => {
  const cargo = seg.search(/\bcargo\b/);
  const tool = cargo < 0 ? null : /\b(?:clippy|rustc)\b/.exec(seg.slice(cargo));
  return tool !== null && /\s["']?-A|\s["']?--allow(?![\w-])|--cap-lints/.test(seg.slice(cargo + tool.index));
}) || /RUST(?:DOC)?FLAGS=(?:"[^"]*?|'[^']*?|\S*?)(?<=[=\s"'\x1f]|\\x1f)(?:-A|--cap-lints)/.test(text);

// cargo add/remove options that take a separate value.
const CARGO_VALUE_FLAGS = new Set(['-p', '--package', '-F', '--features', '--rename', '--registry', '--path', '--git', '--branch', '--tag', '--rev', '--manifest-path', '--target', '--lockfile-path', '--config', '-Z', '--color']);

/** `cargo add`/`cargo remove` invocations and the crate names they touch. */
export function cargoDependencyCommands(cmd) {
  const out = [];
  for (const seg of segments(cmd)) {
    const m = /(?:^|\s)cargo\s+(?:\+\S+\s+)?(add|remove|rm)(?:\s+(.*))?$/.exec(seg.trim());
    if (!m) continue;
    const toks = (m[2] ?? '').trim().split(/\s+/).filter(Boolean);
    const crates = [];
    for (let i = 0; i < toks.length; i++) {
      if (toks[i].startsWith('-')) { if (CARGO_VALUE_FLAGS.has(toks[i])) i++; continue; }
      crates.push(toks[i].split('@')[0]);
    }
    out.push({ op: m[1] === 'rm' ? 'remove' : m[1], crates });
  }
  return out;
}

/** A rule built on scripts/lib/shell.mjs; if reading the command throws, the rule is skipped and the other checks still run. */
const readerRule = (rule, fallback) => { try { return rule(); } catch { return fallback; } };

export function preBash(input) {
  const cmd = String(input.tool_input?.command ?? '');
  const proj = discover(input.cwd);
  if (!proj) return none();
  if (SHELL_WRITE_RS.some((re) => re.test(cmd))) {
    telemetry(proj, input, 'deny-shell-write', {});
    return deny(RS_SHELL_DENY);
  }
  // cp/mv/install onto .rs and git apply/patch: scanned like an Edit/Write (engine-1-8).
  // Only a deny returns at once; an ask or advisory waits for the checks below, and a deny from them wins.
  const rsWrite = input[RS_COPY_SCANNED] ? null : readerRule(() => shellRustWrites(cmd, proj, input), null);
  if (rsWrite) {
    const own = rsWrite.json.hookSpecificOutput.permissionDecision;
    if (own === 'deny') return rsWrite;
    const rest = preBash({ ...input, [RS_COPY_SCANNED]: true });
    const other = rest.json?.hookSpecificOutput?.permissionDecision;
    if (other === 'deny') return rest;
    // The copy's advisories ride along with another check's ask.
    if (other === 'ask' && own !== 'ask') return ask(rest.json.hookSpecificOutput.permissionDecisionReason, [rest.json.hookSpecificOutput.additionalContext, rsWrite.json.hookSpecificOutput.additionalContext].filter(Boolean).join('\n'));
    if (other === 'ask') return ask(`${rsWrite.json.hookSpecificOutput.permissionDecisionReason}\n${rest.json.hookSpecificOutput.permissionDecisionReason}`, rsWrite.json.hookSpecificOutput.additionalContext);
    return rsWrite;
  }
  if (SHELL_WRITE_LOCK.some((re) => re.test(cmd))) {
    telemetry(proj, input, 'deny-lockfile', { via: 'shell' });
    return deny(LOCKFILE_DENY('Cargo.lock'));
  }
  if (SHELL_WRITE_TOML.some((re) => re.test(cmd))) {
    telemetry(proj, input, 'deny-shell-write', { file: 'Cargo.toml' });
    return deny(MANIFEST_SHELL_DENY);
  }
  if (SHELL_WRITE_BASELINE.some((re) => re.test(cmd))) {
    telemetry(proj, input, 'deny-shell-write', { file: 'temper-baseline.json' });
    return deny(BASELINE_SHELL_DENY);
  }
  if (readerRule(() => stateDirWrite(proj, cmd, input.cwd ?? proj.root), false)) {
    telemetry(proj, input, 'deny-shell-write', { file: 'state-dir' });
    return deny(STATE_SHELL_DENY);
  }
  if (/TEMPER_ALLOW_RAISE/.test(cmd)) {
    telemetry(proj, input, 'ask', { via: 'TEMPER_ALLOW_RAISE' });
    return ask(`temper: this command sets TEMPER_ALLOW_RAISE, which lets \`temper baseline --init\`/\`--update\` record every current finding as accepted legacy debt, so the gate and CI stop failing on it. Raising the baseline weakens verification (H-12): approve only if you asked for it.`);
  }
  if (SHELL_WRITE_VENDORED.some((re) => re.test(cmd))) {
    telemetry(proj, input, 'deny-shell-write', { file: '.claude/temper/' });
    return deny(VENDORED_SHELL_DENY);
  }
  const raise = baselineRaise(cmd);
  if (raise) {
    telemetry(proj, input, 'deny-baseline-raise', { via: `baseline ${raise}` });
    return deny(`temper: \`temper baseline ${raise}\` records every current finding as accepted legacy debt (H-12), so it is the human's decision, and the CLI refuses it inside Claude Code. To request it, run the same command prefixed with \`TEMPER_ALLOW_RAISE=1\`: the human will be asked to approve it. (\`baseline --tighten\` and \`--check\` never raise it and run freely.)`);
  }
  const restore = claudeRestore(cmd);
  if (restore) {
    telemetry(proj, input, 'ask', { via: 'git-restore-claude' });
    return ask(`temper: \`${restore.slice(0, 160)}\` puts older verification configuration back into .claude/ (the lint baseline, settings or the vendored CLI). An older baseline can accept more debt (H-12): approve only if you asked for it.`);
  }
  if (segments(cmd).some((seg) => isCargo(seg) && /(?:^|\s)--ignore-rust-version(?![\w-])/.test(seg))) {
    telemetry(proj, input, 'deny-ignore-rust-version', {});
    return deny('temper: --ignore-rust-version bypasses the MSRV check, so green would not mean the declared rust-version builds (MOD-01, H-12). If a dependency needs a newer Rust, raising rust-version is a human decision (H-16): say so and ask.');
  }
  const text = blankProse(cmd);
  if (skipsGitHooks(text)) {
    return deny('temper: skipping git hooks (--no-verify) is not allowed (H-12). Fix what the hook reports.');
  }
  if (lowersLints(text)) {
    return deny('temper: lowering lint levels on the command line is not allowed (H-05). Fix the code the lint points at.');
  }
  // Protected configuration written from the shell asks, as an Edit/Write to it does.
  // After every deny above, so a command that also does something denied stays denied.
  // Its ask is shown together with the dependency and snapshot asks below, as preEdit collects asks.
  const { files: configWrites, unreadPatch } = readerRule(() => protectedShellWrites(proj, cmd, input.cwd), { files: [], unreadPatch: null });
  const what = configWrites.join(', ');
  const configAsks = [
    ...(configWrites.length ? [`temper: this shell command writes ${what}. ${what} ${configWrites.length > 1 ? 'are' : 'is'} verification/agent configuration. Changing ${configWrites.length > 1 ? 'them' : 'it'} can weaken the checks (H-05, H-12). Approve only if you asked for this change.`] : []),
    // A patch from a pipe, a variable or a missing file may change protected configuration too.
    ...(unreadPatch !== null ? [`temper: \`${unreadPatch.slice(0, 120)}\`: temper cannot read the patch, so it cannot tell whether it changes protected configuration (H-05, H-12). Approve only if you know what the patch changes.`] : []),
  ];
  const configAsk = configAsks.length ? configAsks.join('\n') : null;
  if (configWrites.length) telemetry(proj, input, 'ask', { via: 'shell-write-protected', file: configWrites[0] });
  else if (configAsk) telemetry(proj, input, 'ask', { via: 'shell-patch-unread' });
  const withConfig = (reason) => ask(configAsk ? `${configAsk}\n${reason}` : reason);
  if (proj.config.askOnNewDependency) {
    const approved = new Set(proj.config.preapprovedDependencies ?? []);
    const changes = cargoDependencyCommands(cmd).filter((c) => !c.crates.length || c.crates.some((n) => !approved.has(n)));
    if (changes.length) {
      telemetry(proj, input, 'ask', { via: 'cargo-deps' });
      const what = changes.map((c) => `cargo ${c.op} ${c.crates.join(' ') || '(no crate named)'}`).join('; ');
      return withConfig(`temper: \`${what}\` changes the dependency graph without the manifest checks. Adding or removing a dependency is a human decision (H-16, ARC-12). Approve only if you asked for it.`);
    }
  }
  if (/\bcargo\s+insta\s+(?:accept|test\b[^|;&]*--accept)/.test(cmd)) {
    return withConfig('temper: accepting all snapshot changes at once can hide regressions (H-12). Approve only after reviewing the snapshot diffs (cargo insta review).');
  }
  if (configAsk) return ask(configAsk);
  return none();
}

/**
 * PreToolUse SubagentHandback. Where subagents end with a SubagentHandback({ message }) call
 * (Claude Code cloud sessions), the report is that message, and SubagentStop has no
 * last_assistant_message. A reviewer's verdict is checked here, before delivery: an invalid one is
 * denied with the problem list so the reviewer can fix it and hand back again (#97).
 */
export function preHandback(input) {
  const proj = discover(input.cwd);
  if (!proj) return none();
  const type = input.agent_type ?? '';
  telemetry(proj, input, 'handback', { type, reviewer: REVIEWER.test(type) });
  if (!REVIEWER.test(type)) return none();
  const v = extractVerdict(String(input.tool_input?.message ?? ''));
  const problems = checkVerdict(proj, input, v);
  if (problems.length) {
    telemetry(proj, input, 'verdict-invalid', { via: 'handback', problems: problems.length });
    return deny(`temper: the review verdict is invalid, so the report was not delivered:\n- ${problems.join('\n- ')}\nFix the verdict block and hand back again.`);
  }
  storeVerdict(proj, input, v, 'handback');
  return none();
}

/**
 * The subagent's final report from its transcript: the message of its last SubagentHandback call,
 * else the text of its last assistant message. Null when the transcript is missing or unreadable.
 */
export function reportFromTranscript(file) {
  const t = file ? readText(file) : null;
  if (!t) return null;
  const lines = t.split('\n');
  for (let i = lines.length - 1; i >= 0; i--) {
    let rec;
    try { rec = JSON.parse(lines[i]); } catch { continue; }
    const content = rec?.message?.role === 'assistant' ? rec.message.content : null;
    if (!Array.isArray(content)) continue;
    const hb = content.filter((c) => c?.type === 'tool_use' && c.name === 'SubagentHandback').at(-1);
    if (hb) return String(hb.input?.message ?? '');
    const text = content.filter((c) => c?.type === 'text').map((c) => c.text).join('\n');
    if (text.trim()) return text;
  }
  return null;
}

function checkVerdict(proj, input, v) {
  const base = baseRev(proj, input.session_id);
  const gate = v?.fingerprint ? cachedGate(proj, v.fingerprint) : null;
  return validateVerdict(v, { changedRust: rustChanges(proj, base).filter((f) => f.endsWith('.rs')), gate });
}

function storeVerdict(proj, input, v, via) {
  const base = baseRev(proj, input.session_id);
  const current = fingerprint(proj, base);
  recordVerdict(proj, v, { agent_id: input.agent_id, stale: v.fingerprint !== current });
  writeAtomic(path.join(sessionDir(proj, input.session_id), `verdict-${agentKey(input)}`), v.fingerprint);
  telemetry(proj, input, 'verdict', { via, verdict: v.verdict, fp: v.fingerprint, stale: v.fingerprint !== current, findings: (v.findings ?? []).length });
}

// ----------------------------------------------------------------- PostToolUse

export function postTool(input) {
  const ti = input.tool_input ?? {};
  let via = null;
  if (input.tool_name === 'Skill' && SKILL.test(String(ti.skill ?? ti.name ?? ''))) via = 'Skill';
  else if (input.tool_name === 'Read' && /\/rust-craft\/SKILL\.md$/.test(norm(String(ti.file_path ?? '')))) via = 'Read';
  if (via) {
    markSkillLoaded(null, input);
    const proj = discover(input.cwd);
    if (proj) telemetry(proj, input, 'skill-loaded', { via });
  }
  return none();
}

// ------------------------------------------------------------------ Stop gates

function parseStatus(msg) {
  const all = [...String(msg ?? '').matchAll(/^\s*[*_`>-]*\s*Status:\s*[*_`]*\s*(DONE|WIP|BLOCKED)\b/gim)];
  return all.length ? all.at(-1)[1].toUpperCase() : null;
}

function lastBlock(proj, input) {
  const p = path.join(sessionDir(proj, input.session_id), `stop-${agentKey(input)}.json`);
  const t = readText(p);
  let prev = null;
  try { prev = t ? JSON.parse(t) : null; } catch { /* ignore */ }
  return { prev, save: (rec) => writeAtomic(p, JSON.stringify(rec)) };
}

function completionGate(input, { requireReview }) {
  const proj = discover(input.cwd);
  if (!proj || input.permission_mode === 'plan') return none();
  const base = baseRev(proj, input.session_id);
  const changes = rustChanges(proj, base);
  if (!changes.length) return none();
  const startFp = startFingerprint(proj, input.session_id);
  if (startFp && startFp === fingerprint(proj, base)) return none(); // nothing changed by this session
  const status = parseStatus(input.last_assistant_message);
  if (status === 'WIP' || status === 'BLOCKED') {
    telemetry(proj, input, 'stop-declared', { status });
    return notice(`temper: Rust changes left ${status === 'WIP' ? 'in progress' : 'blocked'}; the gate and review were not enforced for this turn.`);
  }
  // gate-start with no Stop outcome after it in the session means the hook was killed (its timeout).
  telemetry(proj, input, 'gate-start', {});
  const t0 = Date.now();
  const res = runGate(proj, { base });
  const timing = { ms: Date.now() - t0, cached: Boolean(res.cached) };
  if (res.unavailable) {
    telemetry(proj, input, 'gate-unavailable', timing);
    return notice(`temper: ${renderGate(res)}`);
  }
  const fp = res.fp;
  const { prev, save } = lastBlock(proj, input);
  const stuck = (kind) => input.stop_hook_active && prev && prev.fp === fp && prev.kind === kind;
  const escape = 'If you cannot finish now, end your message with "Status: WIP" (pausing) or "Status: BLOCKED" and the question for the human.';

  if (!res.ok) {
    if (stuck('gate')) {
      telemetry(proj, input, 'stop-allowed-red', { fp, ...timing });
      return notice(`temper: stopped with a RED gate (no progress after the previous block). ${res.steps.filter((s) => !s.ok).map((s) => s.name).join(', ')} failing.`);
    }
    save({ fp, kind: 'gate', at: Date.now() });
    telemetry(proj, input, 'stop-blocked', { reason: 'gate', fp, ...timing });
    return block(`${renderGate(res)}\n\nThe work is not done while the gate is red. Fix the failures (never by weakening tests or lints, H-12). ${escape}`);
  }
  if (requireReview && proj.config.review !== 'off') {
    const v = verdictFor(proj, fp);
    if (!v || v.verdict !== 'approved') {
      if (proj.config.review === 'advisory') return notice(`temper: gate green; no approving review for fingerprint ${fp} (review is advisory in this repo).`);
      if (stuck('review')) {
        telemetry(proj, input, 'stop-allowed-unreviewed', { fp, ...timing });
        return notice(`temper: stopped without an approving review for fingerprint ${fp}.`);
      }
      save({ fp, kind: 'review', at: Date.now() });
      telemetry(proj, input, 'stop-blocked', { reason: v ? 'changes-requested' : 'no-review', fp, ...timing });
      const why = v
        ? `The reviewer requested changes for this tree (fingerprint ${fp}):\n${summarizeFindings(v)}\nAddress each finding, or rebut it with evidence, then request a fresh review.`
        : `The gate is green, but the current tree (fingerprint ${fp}) has no approving independent review (PROC-08). Delegate to the temper:rust-reviewer subagent with the goal of the change and the plan path. It reviews the diff since ${base ?? 'the session start'} in a fresh context. Address its findings until it approves.`;
      return block(`${why}\n${escape}`);
    }
  }
  telemetry(proj, input, 'stop-green', { fp, ...timing });
  return notice(`temper: gate green${requireReview && proj.config.review === 'required' ? ' and review approved' : ''} (fingerprint ${fp}).`);
}

export function stop(input) {
  const res = completionGate(input, { requireReview: true });
  shipTelemetry(input);
  return res;
}

/**
 * With `telemetry.branch` set, hand this session's telemetry to `temper telemetry push` in a
 * detached process (pushTelemetry), so the Stop hook never waits on the network and a failed push
 * changes nothing in the session. The next Stop pushes again, so a missed push only delays the rows.
 */
function shipTelemetry(input) {
  const proj = discover(input.cwd);
  if (!proj?.config.telemetry?.branch || !proj.gitRoot || !input.session_id) return;
  try {
    spawn(process.execPath, [path.join(path.dirname(fileURLToPath(import.meta.url)), '..', 'temper.mjs'), 'telemetry', 'push', '--session', String(input.session_id)], {
      cwd: proj.root, detached: true, stdio: 'ignore', windowsHide: true,
    }).unref();
  } catch { /* best effort, like telemetry itself */ }
}

export function subagentStop(input) {
  const type = input.agent_type ?? '';
  const proj = discover(input.cwd);
  if (!proj) return none();
  // Before any filtering, so a type the patterns miss still shows in telemetry (#97).
  telemetry(proj, input, 'subagent-stop', { type, msg: typeof input.last_assistant_message === 'string', transcript: Boolean(input.agent_transcript_path) });
  if (REVIEWER.test(type)) {
    const already = readText(path.join(sessionDir(proj, input.session_id), `verdict-${agentKey(input)}`));
    // A subagent that hands back (cloud sessions) has no last_assistant_message: read its report from the transcript.
    const report = input.last_assistant_message ?? reportFromTranscript(input.agent_transcript_path);
    const v = extractVerdict(report);
    if (!v && already) return none(); // recorded by SubagentHandback or an earlier SubagentStop for this agent
    const problems = checkVerdict(proj, input, v);
    if (problems.length) {
      if (input.stop_hook_active) { telemetry(proj, input, 'verdict-invalid-final', { problems: problems.length }); return none(); }
      telemetry(proj, input, 'verdict-invalid', { via: 'subagent-stop', problems: problems.length });
      return block(`temper: your review verdict is invalid:\n- ${problems.join('\n- ')}\nEnd your final report with a corrected \`\`\`json verdict block.`);
    }
    storeVerdict(proj, input, v, 'subagent-stop');
    return none();
  }
  if (IMPLEMENTER.test(type)) {
    // Its "Status:" line is in the hand-back report when there is no last_assistant_message.
    const report = input.last_assistant_message ?? reportFromTranscript(input.agent_transcript_path);
    return completionGate({ ...input, last_assistant_message: report }, { requireReview: false });
  }
  return none();
}

// -------------------------------------------------------------- SubagentStart

export function subagentStart(input) {
  const type = input.agent_type ?? '';
  const proj = discover(input.cwd);
  if (!proj) return none();
  telemetry(proj, input, 'subagent-start', { type }); // before filtering (#97)
  if (!PRELOADED.test(type)) return none();
  const base = baseRev(proj, input.session_id);
  if (REVIEWER.test(type)) run('cargo', ['fmt', '--all'], { cwd: proj.root, timeoutMs: 120000 });
  const changes = rustChanges(proj, base);
  const fp = fingerprint(proj, base);
  const g = cachedGate(proj, fp);
  const plans = listPlans(proj);
  const lines = [
    `temper context for ${type}:`,
    `- Project root: ${proj.root}`,
    `- Base revision: ${base ?? 'none (no git)'}; review scope: \`git diff ${base ?? ''}\` plus untracked files.`,
    `- Current tree fingerprint: ${fp}`,
    `- Changed Rust files (${changes.length}): ${changes.slice(0, 40).join(', ') || 'none'}`,
    `- Gate for this fingerprint: ${g ? (g.ok ? 'green' : `red (${g.steps.filter((s) => !s.ok).map((s) => s.name).join(', ')})`) : 'not run yet'}. Gate command: ${gateCmd()}`,
    plans.length ? `- Plans: ${plans.join(', ')}` : '- Plans: none under .temper/plans/',
  ];
  if (REVIEWER.test(type)) {
    lines.push(`- Your verdict's "fingerprint" field must be ${fp}. If the tree changes during your review the verdict is recorded as stale and a new review will be needed.`);
    const stat = diffStat(proj, base);
    if (stat) lines.push(`- Diff stat:\n${stat.split('\n').slice(-25).join('\n')}`);
  }
  return ctx('SubagentStart', lines.join('\n'));
}

function listPlans(proj) {
  const dir = path.join(proj.root, '.temper', 'plans');
  try {
    return fs.readdirSync(dir).filter((f) => f.endsWith('.md'))
      .map((f) => ({ f, t: fs.statSync(path.join(dir, f)).mtimeMs }))
      .sort((a, b) => b.t - a.t).slice(0, 5).map((x) => `.temper/plans/${x.f}`);
  } catch { return []; }
}
