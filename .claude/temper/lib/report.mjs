// temper report: the compliance scorecard for engineers and managers.
//
//   temper report [--findings FILE] [--base REF] [--out DIR] [--format json,md,html] [--locked]
//
// Input, first found: --findings FILE, the gate's cached collection for this
// fingerprint (<stateDir>/findings-<fp>.json), or a fresh collection.
// New, legacy, fixed and raised come from one place, the ratchet: the result
// `baseline --check --findings-out` embeds in FILE (used as-is), or else a
// ratchet() run here against the committed baseline (with --base: the lines
// changed since REF; without: the totals).
//
// Output in DIR (default target/temper-report; like --findings-out, only under
// target/ or .temper/): report.json (everything), report.md (at most 56 lines,
// for $GITHUB_STEP_SUMMARY and a PR comment) and report.html (one
// self-contained page: inline CSS, no JS, no requests).
//
// buildReport, renderMarkdown and renderHtml are pure, for tests.
import fs from 'node:fs';
import path from 'node:path';
import { discover, fingerprint, headRev, readText, norm, relTo, resolveRev, safeOutputPath } from './project.mjs';
import { toolOf, ruleMap, collectFindings, cachedFindings, isComplete } from './findings.mjs';
import { readBaseline, ratchet, crateAliases, BASELINE_PATH } from './baseline.mjs';
import { temperVersion } from './vendor.mjs';

export const DEFAULT_OUT = 'target/temper-report';
export const FORMATS = ['json', 'md', 'html'];
// The spec caps the Markdown at 60 lines; CI appends a link line to the PR comment.
export const MD_MAX_LINES = 56;
export const REPORT_MARKER = '<!-- temper-report -->';
const SEVERITIES = ['must', 'should', 'consider', 'unmapped'];
const NO_CRATE = '(none)'; // the baseline's key for findings outside every crate
const ALL_SOURCES = ['clippy', 'rustdoc', 'scan'];
const STATUS_RANK = { regressed: 0, debt: 1, clean: 2 };
const LABEL = { clean: 'clean', debt: 'debt', regressed: 'regressed', failing: 'failing', error: 'build errors' };
const GLYPH = { clean: '✓', debt: '!', regressed: '✕', failing: '✕', error: '✕' };

const USAGE = 'usage: temper report [--findings FILE] [--base REF] [--out DIR] [--format json,md,html] [--locked]';

// ---------- the model ----------

const bump = (m, k, n = 1) => m.set(k, (m.get(k) ?? 0) + n);
const byCount = (a, b) => b[1] - a[1] || String(a[0]).localeCompare(String(b[0]));
const pairKey = (crate, lint) => `${crate}\0${lint}`;

/**
 * Aggregate a findings collection into the report model (the report.json content).
 * The ratchet result is the one definition of new, legacy, fixed and raised:
 * pass the one `baseline --check` computed (or `ratchet()` with a base); with
 * none, the totals are compared here (no git). Approved lint exceptions
 * (scanner `ask` hits carrying `exception`) are not debt.
 * @param {object} collection `{ temper_findings: 1, at, commit, toolchain, sources, crates, findings, errors }`
 * @param {object|null} baseline `{ temper_baseline: 1, totals: { crate: { lint: n } }, crates, ... }` or null
 * @param {object|null} ratchetResult `ratchet()` output
 * @param {{ generated?: string, version?: string, base?: string, source?: string, title?: string, rulemap?: object }} [meta]
 */
export function buildReport(collection, baseline, ratchetResult, meta = {}) {
  const ruleInfo = (meta.rulemap ?? ruleMap()).rules ?? {};
  const all = Array.isArray(collection?.findings) ? collection.findings : [];
  const findings = all.filter((f) => !f.exception);
  const exceptions = all.filter((f) => f.exception);
  const errors = Array.isArray(collection?.errors) ? collection.errors : [];
  const sources = Array.isArray(collection?.sources) ? collection.sources : ALL_SOURCES;
  const res = ratchetResult ?? ratchet({ ...collection, findings: all, errors }, baseline, { changed: null });
  const strict = res.mode === 'strict';
  const crateOf = (f) => f.crate ?? NO_CRATE;
  // The ratchet keys a renamed crate (same directory) by its baseline name; rows use the current one.
  const alias = baseline ? crateAliases(baseline, collection?.crates) : new Map();
  const current = new Map([...alias].map(([now, old]) => [old, now]));
  const keyOf = (f) => alias.get(crateOf(f)) ?? crateOf(f);
  const nameOf = (c) => current.get(c) ?? c;
  const sev = (f) => (SEVERITIES.includes(f.severity) ? f.severity : 'unmapped');
  const isH = (f) => (f.rules ?? []).some((r) => /^H-\d+$/.test(r));

  const rows = new Map();
  const row = (name) => {
    if (!rows.has(name)) {
      rows.set(name, {
        name, dir: null, in_workspace: false, lints_inherited: null, charter: null,
        must: 0, should: 0, consider: 0, unmapped: 0, other: 0, total: 0, h: 0, new: 0, fixed: 0, baseline_total: strict ? null : 0,
        status: 'clean', top_rules: [], _rules: new Map(),
      });
    }
    return rows.get(name);
  };
  for (const c of collection?.crates ?? []) {
    const r = row(c.name);
    Object.assign(r, { dir: c.dir ?? null, in_workspace: true, lints_inherited: c.lints_inherited ?? null, charter: c.charter ?? null });
  }

  const ruleCount = new Map();
  const unmappedLints = new Map();
  const files = new Map();
  const bySeverity = { must: 0, should: 0, consider: 0, unmapped: 0 };
  let hTotal = 0;
  for (const f of findings) {
    const r = row(crateOf(f));
    const s = sev(f);
    r[s] += 1; r.total += 1; bySeverity[s] += 1;
    if (isH(f)) { r.h += 1; hTotal += 1; }
    for (const id of new Set(f.rules ?? [])) { bump(r._rules, id); bump(ruleCount, id); }
    if (s === 'unmapped') bump(unmappedLints, f.lint);
    const key = f.file ?? '(no file)';
    if (!files.has(key)) files.set(key, { file: key, crate: crateOf(f), must: 0, h: 0, total: 0 });
    const fa = files.get(key);
    fa.total += 1;
    if (s === 'must') fa.must += 1;
    if (isH(f)) fa.h += 1;
  }

  // New per (crate, lint): the findings on changed lines, or the count rise when that is larger.
  const newCode = Array.isArray(res.new_code) ? res.new_code : [];
  const increases = Array.isArray(res.increases) ? res.increases : [];
  const fresh = new Map();
  for (const f of newCode) bump(fresh, pairKey(keyOf(f), f.lint));
  const rise = new Map(increases.map((x) => [pairKey(x.crate, x.lint), x.current - x.baseline]));
  for (const k of new Set([...fresh.keys(), ...rise.keys()])) {
    row(nameOf(k.split('\0')[0])).new += Math.max(fresh.get(k) ?? 0, rise.get(k) ?? 0);
  }
  for (const x of res.fixed ?? []) row(nameOf(x.crate)).fixed += x.baseline - x.current;
  if (baseline && !strict) {
    for (const [c, lints] of Object.entries(baseline.totals ?? {})) {
      const r = row(nameOf(c));
      for (const [l, n] of Object.entries(lints)) if (sources.includes(toolOf(l))) r.baseline_total += Number(n) || 0;
    }
  }
  const renamed = (x) => ({ ...x, crate: nameOf(x.crate) });
  // Increases the new-code findings do not explain: counts that rose on lines the change did not touch.
  const untouched = increases.filter((x) => x.current - (fresh.get(pairKey(x.crate, x.lint)) ?? 0) > x.baseline).map(renamed);

  const crates = [...rows.values()].map((r) => {
    const status = r.new > 0 ? 'regressed' : r.must === 0 ? 'clean' : 'debt';
    const top = [...r._rules].sort(byCount).slice(0, 3).map(([id, count]) => ({ id, count }));
    const { _rules, ...rest } = r;
    return { ...rest, other: r.consider + r.unmapped, status, top_rules: top };
  }).sort((a, b) => (b.in_workspace - a.in_workspace) || STATUS_RANK[a.status] - STATUS_RANK[b.status]
    || b.must - a.must || b.total - a.total || a.name.localeCompare(b.name));
  const inWs = crates.filter((c) => c.in_workspace || c.total > 0);
  const cratesSummary = {
    total: inWs.length,
    clean: inWs.filter((c) => c.status === 'clean').length,
    debt: inWs.filter((c) => c.status === 'debt').length,
    regressed: inWs.filter((c) => c.status === 'regressed').length,
  };

  const ruleCounts = [...ruleCount].sort(byCount).map(([id, count]) => ({
    id, count, level: ruleInfo[id]?.level ?? null, family: ruleInfo[id]?.family ?? id.replace(/-\d+$/, ''), title: ruleInfo[id]?.title ?? null,
  }));
  const fileCounts = [...files.values()].sort((a, b) => b.must - a.must || b.total - a.total || a.file.localeCompare(b.file));

  let status;
  if (errors.length || res.errors?.length) status = 'error';
  else if (strict) status = res.ok ? 'clean' : 'failing';
  else if (!res.ok) status = 'regressed';
  else status = findings.length ? 'debt' : 'clean';

  const report = {
    temper_report: 1,
    title: meta.title ?? 'Rust workspace',
    status,
    status_text: '',
    totals: {
      findings: findings.length, ...bySeverity, h: hTotal, errors: errors.length, exceptions: exceptions.length,
      new: crates.reduce((n, c) => n + c.new, 0),
      fixed: strict ? null : res.fixed_count ?? 0,
      legacy: strict ? null : res.legacy_total ?? 0,
      baseline: strict ? null : res.baseline_total ?? 0,
      new_code: newCode.length,
      raised: (res.raised ?? []).length,
    },
    crates_summary: cratesSummary,
    baseline: baseline && !strict ? {
      present: true, total: res.baseline_total ?? null, created: baseline.created ?? null, updated: baseline.updated ?? null,
      commit: baseline.commit ?? null, toolchain: baseline.toolchain ?? null, host: baseline.host ?? null,
    } : { present: !strict },
    // mode: strict (no baseline: every finding is new), diff+totals (changed lines and totals), totals.
    ratchet: {
      mode: res.mode, ok: res.ok, base: meta.base ?? res.base ?? null, merge_base: res.base ?? null,
      notes: strict ? [] : res.notes ?? [], computed_by: ratchetResult ? meta.ratchetFrom ?? 'caller' : 'buildReport (totals only)',
    },
    sources,
    crates,
    top_rules: ruleCounts.slice(0, 15),
    top_files: fileCounts.slice(0, 15),
    new_code: newCode.map((f) => ({
      crate: crateOf(f), file: f.file ?? null, line: f.line ?? null, lint: f.lint, rules: f.rules ?? [], severity: sev(f), message: f.message ?? '',
    })),
    increases: increases.map(renamed),
    untouched_increases: untouched,
    fixed: (res.fixed ?? []).map(renamed),
    raised: res.raised ?? [],
    unmapped_lints: [...unmappedLints].sort(byCount).map(([lint, count]) => ({ lint, count })),
    exceptions: exceptions.map((f) => ({ crate: crateOf(f), file: f.file ?? null, line: f.line ?? null, lint: f.lint, exception: f.exception })),
    errors,
    rule_counts: ruleCounts,
    file_counts: fileCounts,
    provenance: {
      generated: meta.generated ?? new Date().toISOString(),
      collected: collection?.at ?? null,
      commit: collection?.commit ?? null,
      toolchain: collection?.toolchain ?? null,
      temper: meta.version ?? null,
      base: res.mode === 'diff+totals' ? meta.base ?? res.base ?? null : null,
      source: meta.source ?? null,
    },
  };
  report.status_text = statusText(report);
  return report;
}

const fmt = (n) => (n === null || n === undefined ? '–' : Number(n).toLocaleString('en-US'));
const plural = (n, one, many = `${one}s`) => `${fmt(n)} ${n === 1 ? one : many}`;
const short = (sha) => (sha ? String(sha).slice(0, 12) : null);
/** A base for display: a ref as given, a commit sha shortened. */
const baseLabel = (b) => (b && /^[0-9a-f]{40}$/.test(b) ? b.slice(0, 12) : b);

/** Did the ratchet compare changed lines (not only the totals)? */
const diffed = (r) => r.ratchet.mode === 'diff+totals';

/** One or two sentences under the status badge; `code` in backticks. */
function statusText(r) {
  const t = r.totals;
  const base = diffed(r) && r.ratchet.base ? `\`${baseLabel(r.ratchet.base)}\`` : null;
  // --tighten helps only on a passing ratchet with fixes.
  const fixedNote = r.ratchet.ok && t.fixed ? ` ${plural(t.fixed, 'finding')} fixed since the baseline: run \`temper baseline --tighten\` to lock that in.` : '';
  switch (r.status) {
    case 'error':
      return `${plural(t.errors, 'compile error')}: the build is broken, so the lint results below are incomplete.`;
    case 'failing':
      return `No baseline, so zero findings are allowed, and ${plural(t.new, 'finding')} fail the gate. Fix them, or have a human accept the current debt with \`temper baseline --init\`.`;
    case 'regressed': {
      const parts = [];
      if (t.new_code) parts.push(`${fmt(t.new_code)} on ${base ? 'changed lines' : 'lines that are never legacy'}`);
      if (r.untouched_increases.length) parts.push(`${plural(r.untouched_increases.length, 'crate/lint count')} above the baseline on untouched lines`);
      return `${plural(t.new, 'new finding')}${base ? ` since ${base}` : ' above the baseline'}${parts.length ? ` (${parts.join('; ')})` : ''}: the ratchet fails. ${plural(t.legacy, 'legacy finding')} stay within the baseline.`;
    }
    case 'debt':
      return `Nothing new${base ? ` since ${base}` : ' against the baseline totals'}. ${plural(t.legacy, 'legacy finding')} (${fmt(t.must)} MUST) are within the baseline of ${fmt(t.baseline)}.${fixedNote}`;
    default:
      return r.baseline.present && t.baseline > 0
        ? `No findings. The baseline still allows ${plural(t.baseline, 'finding')}: run \`temper baseline --tighten\` to lock that in.`
        : 'No findings: every crate is clean.';
  }
}

/** The raised-baseline headline, shared by the Markdown and the HTML. */
function raisedText(r) {
  const since = r.ratchet.merge_base ? ` since \`${baseLabel(r.ratchet.merge_base)}\`` : '';
  return `${plural(r.raised.length, 'count')} higher in \`${BASELINE_PATH}\`${since}: the accepted debt grew. Raising the baseline is a human decision (H-12); check that this change was meant to.`;
}

// ---------- Markdown ----------

/**
 * Markdown-safe text: `code` spans are kept (clippy quotes code in them) and
 * everything outside them is escaped, so a diagnostic cannot inject markup,
 * links or @-mentions into a PR comment.
 */
function mdText(s, max = 0) {
  let str = String(s ?? '').replace(/\s*\r?\n\s*/g, ' ');
  if (max && str.length > max) {
    str = str.slice(0, max - 1);
    // Never cut inside a code span: drop the partial one.
    if ((str.match(/`/g) ?? []).length % 2) str = str.slice(0, str.lastIndexOf('`')).trimEnd();
    str += '…';
  }
  const outside = (p) => p.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
    .replace(/([\\*_[\]~#!])/g, '\\$1').replace(/@/g, '@\u200b');
  const parts = str.split('`');
  if (parts.length % 2 === 0) return outside(str).replace(/`/g, '&#96;');
  return parts.map((p, i) => (i % 2 ? `\`${p}\`` : outside(p))).join('');
}
const mdCell = (s, max) => mdText(s, max).replace(/\|/g, '\\|');
const mdCode = (s) => `\`${String(s ?? '').replace(/[`\r\n]/g, '')}\``;
const loc = (f) => (f.file ? `${f.file}${f.line ? `:${f.line}` : ''}` : '(no file)');
const ruleList = (rules) => (rules?.length ? rules.join(', ') : 'unmapped');

/** The compact summary: at most MD_MAX_LINES lines, details left to report.html. */
export function renderMarkdown(r) {
  const t = r.totals;
  const cs = r.crates_summary;
  const strict = r.ratchet.mode === 'strict';
  // A raised baseline is always shown in full view: it is the one thing a reviewer must not miss.
  const raised = r.raised.length ? [
    '',
    `> **BASELINE RAISED.** ${mdText(raisedText(r))}`,
    ...r.raised.slice(0, 5).map((x) => `> - ${mdCode(x.crate)} ${mdCode(x.lint)}: ${fmt(x.from)} → ${fmt(x.to)}`),
    ...(r.raised.length > 5 ? [`> - … ${fmt(r.raised.length - 5)} more in report.html`] : []),
  ] : [];
  const head = [
    REPORT_MARKER,
    `### temper compliance: ${LABEL[r.status]}${r.raised.length ? ' · baseline raised' : ''}`,
    '',
    `**${LABEL[r.status].toUpperCase()}.** ${mdText(r.status_text)}`,
    ...raised,
    ...r.ratchet.notes.slice(0, 2).flatMap((n) => ['', `_Note: ${mdText(n, 300)}_`]),
    '',
    '| MUST findings | H-rule findings | New | Fixed | Crates clean |',
    '|---:|---:|---:|---:|:---|',
    `| ${fmt(t.must)} of ${fmt(t.findings)} | ${fmt(t.h)} | ${fmt(t.new)}${strict ? ' (no baseline)' : ''} | ${fmt(t.fixed)} | ${cs.clean} of ${cs.total} (${cs.debt} debt, ${cs.regressed} regressed) |`,
  ];
  const sections = [];
  // Each section starts at `cap` rows and may shrink to `min` to fit the line budget.
  const tableSection = (title, header, rows, min, cap) => ({ kind: 'table', title, header, rows, min, cap });
  const listSection = (title, rows, min, cap) => ({ kind: 'list', title, rows, min, cap });
  if (r.errors.length) {
    sections.push(listSection((n, all) => `**Compile errors** (${shown(n, all)})`, r.errors.map((e) => `- ${mdText(errorText(e), 160)}`), 1, 5));
  }
  if (r.crates.length) {
    sections.push(tableSection(
      (n, all) => `**Crates** (${shown(n, all)}; clean: no MUST findings, debt: MUST findings within the baseline, regressed: new findings)`,
      ['| Crate | Status | MUST | SHOULD | Other | H | New | Fixed | Top rules |', '|---|---|---:|---:|---:|---:|---:|---:|---|'],
      r.crates.map((c) => `| ${mdCode(c.name)} | ${LABEL[c.status]} | ${fmt(c.must)} | ${fmt(c.should)} | ${fmt(c.other)} | ${fmt(c.h)} | ${fmt(c.new)} | ${strict ? '–' : fmt(c.fixed)} | ${c.top_rules.map((x) => `${x.id} (${x.count})`).join(', ') || '–'} |`),
      3, 12,
    ));
  }
  if (r.top_rules.length) {
    sections.push(tableSection(
      (n, all) => `**Top rules** (${shown(n, all)})`,
      ['| Rule | Level | Count | Rule text |', '|---|---|---:|---|'],
      r.top_rules.map((x) => `| ${x.id} | ${x.level ?? '–'} | ${fmt(x.count)} | ${mdCell(x.title ?? '', 90)} |`),
      3, 8,
    ));
  }
  if (r.new_code.length) {
    sections.push(listSection(
      (n, all) => `**${strict ? 'Findings' : 'New-code findings'}** (${shown(n, all)}, ${newCodeWhere(r, mdCode)})`,
      r.new_code.map((f) => `- ${mdCode(loc(f))} ${mdCode(f.lint)} (${ruleList(f.rules)}): ${mdText(f.message, 140)}`),
      3, 10,
    ));
  }
  if (r.untouched_increases.length) {
    sections.push(listSection(
      (n, all) => `**Counts above the baseline on untouched lines** (${shown(n, all)}; a change can make unchanged code warn)`,
      r.untouched_increases.map((x) => `- ${mdCode(x.crate)} ${mdCode(x.lint)}: ${fmt(x.baseline)} → ${fmt(x.current)}`),
      1, 5,
    ));
  }
  const prov = r.provenance;
  const foot = ['', `<sub>temper ${mdText(prov.temper ?? '?')} · commit ${mdText(short(prov.commit) ?? 'n/a')} · toolchain ${mdText(prov.toolchain ?? 'n/a')} · generated ${mdText(isoMinute(prov.generated))} · details in report.html</sub>`];

  // Fit: shrink the longest section until the whole summary fits.
  const overhead = (s) => (s.kind === 'table' ? 5 : 3);
  const caps = sections.map((s) => Math.min(s.rows.length, s.cap));
  const total = () => head.length + foot.length + sections.reduce((n, s, i) => n + (caps[i] ? overhead(s) + caps[i] : 0), 0);
  while (total() > MD_MAX_LINES) {
    let i = -1;
    sections.forEach((s, j) => { if (caps[j] > Math.min(s.min, s.rows.length) && (i < 0 || caps[j] > caps[i])) i = j; });
    if (i < 0) break;
    caps[i] -= 1;
  }
  const out = [...head];
  sections.forEach((s, i) => {
    if (!caps[i]) return;
    out.push('', s.title(caps[i], s.rows.length), '');
    if (s.kind === 'table') out.push(...s.header);
    out.push(...s.rows.slice(0, caps[i]));
  });
  out.push(...foot);
  return `${out.join('\n')}\n`;
}

const shown = (n, all) => (n < all ? `${fmt(n)} of ${fmt(all)}` : fmt(all));
/** Which findings the new-code list holds, for its heading. */
function newCodeWhere(r, code) {
  if (r.ratchet.mode === 'strict') return 'no baseline, so each one fails the gate';
  if (diffed(r)) return `on lines changed since ${code(baseLabel(r.ratchet.base ?? 'the base'))}`;
  return 'lints that are never accepted as legacy';
}
const isoMinute = (iso) => (iso ? `${String(iso).slice(0, 16).replace('T', ' ')} UTC` : 'n/a');
function errorText(e) {
  if (typeof e === 'string') return e;
  const where = e.file ? `${e.file}${e.line ? `:${e.line}` : ''} ` : '';
  return `${where}${e.code ? `${e.code} ` : ''}${e.message ?? JSON.stringify(e)}`;
}

// ---------- HTML ----------

const esc = (s) => String(s ?? '').replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#39;');
/** Escaped text with `code` spans rendered as <code>. */
const rich = (s) => esc(s).replace(/`([^`]+)`/g, '<code>$1</code>');
const clip = (s, max) => { const str = String(s ?? ''); return str.length > max ? `${str.slice(0, max - 1)}…` : str; };
const num = (n) => `<td class="num">${esc(fmt(n))}</td>`;
const pill = (status) => `<span class="pill pill-${esc(status)}"><span class="glyph" aria-hidden="true">${GLYPH[status]}</span>${esc(LABEL[status])}</span>`;
const yesNo = (v) => (v === true ? 'yes' : v === false ? '<span class="warn-text">no</span>' : '–');

const CSS = `
:root {
  color-scheme: light;
  --page: #f6f6f3;
  --surface: #ffffff;
  --ink: #0b0b0b;
  --ink-2: #52514e;
  --ink-3: #6b6a65;
  --line: #e1e0d9;
  --code-bg: #efeee9;
  --good: #0ca30c; --good-ink: #006300; --good-bg: #e5f3e5;
  --warn: #fab219; --warn-ink: #7a4d00; --warn-bg: #fdf0d2;
  --crit: #d03b3b; --crit-ink: #a51f1f; --crit-bg: #fbe7e7;
  --sev-must: #1c5cab; --sev-should: #5598e7; --sev-consider: #9ec5f4; --sev-unmapped: #c3c2b7;
  --bar: #86b6ef;
  --font: system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
  --mono: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace;
}
@media (prefers-color-scheme: dark) {
  :root {
    color-scheme: dark;
    --page: #0d0d0d;
    --surface: #1a1a19;
    --ink: #ffffff;
    --ink-2: #c3c2b7;
    --ink-3: #a3a29a;
    --line: #2f2f2c;
    --code-bg: #2a2a27;
    --good-ink: #5fd35f; --good-bg: #122a14;
    --warn-ink: #fbbf3c; --warn-bg: #30250b;
    --crit-ink: #f58a8a; --crit-bg: #361515;
    --sev-must: #9ec5f4; --sev-should: #5598e7; --sev-consider: #256abf; --sev-unmapped: #52514e;
    --bar: #3987e5;
  }
}
* { box-sizing: border-box; }
html { -webkit-text-size-adjust: 100%; text-size-adjust: 100%; }
body { margin: 0; background: var(--page); color: var(--ink); font: 15px/1.5 var(--font); }
.page { max-width: 1120px; margin: 0 auto; padding: 28px 16px 48px; }
h1 { font-size: 26px; line-height: 1.2; margin: 2px 0 6px; overflow-wrap: anywhere; }
h2 { font-size: 18px; margin: 36px 0 4px; }
p { margin: 0; }
a { color: inherit; text-underline-offset: 2px; }
code { font-family: var(--mono); font-size: .88em; background: var(--code-bg); padding: 1px 5px; border-radius: 5px; overflow-wrap: break-word; }
td.path code { overflow-wrap: anywhere; }
.eyebrow { font-size: 13px; font-weight: 600; letter-spacing: .04em; text-transform: uppercase; color: var(--ink-2); }
.meta, .hint { font-size: 13.5px; color: var(--ink-2); }
.hint { margin-bottom: 10px; }
.status { display: flex; flex-wrap: wrap; gap: 8px 14px; align-items: baseline; margin-top: 18px; padding: 14px 16px;
  border-radius: 12px; border: 1px solid var(--line); border-left: 5px solid var(--st); background: var(--st-bg); }
.status p { flex: 1 1 320px; }
.status-clean { --st: var(--good); --st-bg: var(--good-bg); }
.status-debt { --st: var(--warn); --st-bg: var(--warn-bg); }
.status-regressed, .status-failing, .status-error { --st: var(--crit); --st-bg: var(--crit-bg); }
.pill { display: inline-flex; align-items: center; gap: 5px; padding: 1px 9px 1px 7px; border-radius: 999px; font-size: 13px;
  font-weight: 650; white-space: nowrap; border: 1px solid currentColor; }
.status .pill { font-size: 14px; text-transform: uppercase; letter-spacing: .03em; background: var(--surface); }
.glyph { font-size: .95em; }
.pill-clean { color: var(--good-ink); }
.pill-debt { color: var(--warn-ink); }
.pill-regressed, .pill-failing, .pill-error { color: var(--crit-ink); }
.kpis { display: grid; grid-template-columns: repeat(auto-fit, minmax(160px, 1fr)); gap: 12px; margin-top: 14px; }
.kpi { background: var(--surface); border: 1px solid var(--line); border-radius: 12px; padding: 12px 14px 13px; }
.kpi-alert { border-top: 4px solid var(--crit); }
.kpi-label { font-size: 13.5px; color: var(--ink-2); }
.kpi-value { font-size: 30px; font-weight: 650; line-height: 1.15; margin: 4px 0 2px; }
.kpi-value small { font-size: 17px; font-weight: 600; color: var(--ink-2); }
.kpi-sub { font-size: 13px; color: var(--ink-2); }
.sev { margin-top: 12px; background: var(--surface); border: 1px solid var(--line); border-radius: 12px; padding: 12px 14px; }
.sevbar { display: flex; gap: 2px; height: 10px; border-radius: 5px; overflow: hidden; background: var(--line); }
.sevbar span { display: block; min-width: 3px; }
.legend { display: flex; flex-wrap: wrap; gap: 4px 18px; list-style: none; padding: 0; margin: 10px 0 0; font-size: 13.5px; color: var(--ink-2); }
.legend b { color: var(--ink); font-variant-numeric: tabular-nums; }
.sw { display: inline-block; width: 10px; height: 10px; border-radius: 3px; margin-right: 6px; vertical-align: -1px; }
.s-must { background: var(--sev-must); } .s-should { background: var(--sev-should); }
.s-consider { background: var(--sev-consider); } .s-unmapped { background: var(--sev-unmapped); }
.compare { margin-top: 8px; font-size: 13.5px; color: var(--ink-2); }
.compare b { color: var(--ink); font-variant-numeric: tabular-nums; }
.table-wrap { overflow-x: auto; -webkit-overflow-scrolling: touch; background: var(--surface); border: 1px solid var(--line); border-radius: 12px; }
table { border-collapse: collapse; width: 100%; font-size: 14px; }
th, td { padding: 8px 12px; text-align: left; vertical-align: top; border-bottom: 1px solid var(--line); }
tbody tr:last-child td { border-bottom: 0; }
th { font-size: 12.5px; font-weight: 600; color: var(--ink-2); white-space: nowrap; background: var(--surface); }
td.num, th.num { text-align: right; font-variant-numeric: tabular-nums; white-space: nowrap; }
td.nowrap { white-space: nowrap; }
td.muted, .muted { color: var(--ink-3); }
.warn-text { color: var(--warn-ink); font-weight: 600; }
.scorecard { min-width: 900px; }
.scorecard th:first-child, .scorecard td:first-child { position: sticky; left: 0; background: var(--surface); box-shadow: 1px 0 0 var(--line); }
.rules { min-width: 680px; }
.rules td.text { min-width: 300px; }
.files { min-width: 560px; }
.findings { min-width: 760px; }
.pairs { min-width: 480px; }
td.path { min-width: 220px; }
td.msg { min-width: 280px; }
.lvl { display: inline-block; font-size: 11.5px; font-weight: 650; letter-spacing: .04em; padding: 0 6px; border-radius: 4px; border: 1px solid var(--line); color: var(--ink-2); }
.lvl-MUST { color: var(--ink); border-color: var(--ink-3); }
.count { width: 130px; }
.count-in { display: flex; align-items: center; justify-content: flex-end; gap: 10px; }
.track { flex: 0 0 64px; height: 6px; border-radius: 3px; background: var(--line); overflow: hidden; }
.bar { display: block; height: 100%; border-radius: 3px; background: var(--bar); min-width: 3px; }
.count-in .n { min-width: 4ch; text-align: right; }
.more { margin-top: 8px; font-size: 13px; color: var(--ink-2); }
.errors { margin: 8px 0 0; padding-left: 20px; font-size: 14px; }
.notes { margin: 8px 0 0; padding-left: 20px; font-size: 13.5px; color: var(--ink-2); }
.raised { margin-top: 12px; padding: 14px 16px; border-radius: 12px; border: 1px solid var(--line); border-left: 5px solid var(--warn); background: var(--warn-bg); }
.raised-head { display: flex; flex-wrap: wrap; gap: 8px 14px; align-items: baseline; margin-bottom: 12px; }
.raised-head > span:last-child { flex: 1 1 320px; }
.pill-raised { color: var(--warn-ink); background: var(--surface); font-size: 14px; text-transform: uppercase; letter-spacing: .03em; }
.errors li { margin: 2px 0; overflow-wrap: anywhere; }
footer { margin-top: 40px; padding-top: 14px; border-top: 1px solid var(--line); }
.prov { display: grid; grid-template-columns: max-content 1fr; gap: 3px 16px; margin: 6px 0 0; font-size: 13px; color: var(--ink-2); }
.prov dt { font-weight: 600; }
.prov dd { margin: 0; overflow-wrap: anywhere; }
@media (max-width: 520px) {
  .page { padding-top: 20px; }
  h1 { font-size: 22px; }
  .kpis { grid-template-columns: 1fr 1fr; gap: 10px; }
  .kpi-value { font-size: 26px; }
  .kpi:last-child:nth-child(odd) { grid-column: 1 / -1; }
  /* Top rules as cards: id, level and count on one line, the rule text below. */
  .rules, .rules tbody { display: block; min-width: 0; }
  .rules thead { display: none; }
  .rules tr { display: grid; grid-template-columns: auto auto 1fr; align-items: center; column-gap: 10px; padding: 10px 12px; border-bottom: 1px solid var(--line); }
  .rules tbody tr:last-child { border-bottom: 0; }
  .rules td { padding: 0; border: 0; }
  .rules td.count { width: auto; }
  .rules td.text { grid-column: 1 / -1; min-width: 0; margin-top: 4px; }
}
`;

function table(cls, head, rows) {
  return `<div class="table-wrap"><table class="${cls}"><thead><tr>${head.map(([t, c]) => `<th scope="col"${c ? ` class="${c}"` : ''}>${t}</th>`).join('')}</tr></thead>`
    + `<tbody>${rows.join('')}</tbody></table></div>`;
}

/** One self-contained page: inline CSS, no scripts, no external requests; every diagnostic string escaped. */
export function renderHtml(r) {
  const t = r.totals;
  const cs = r.crates_summary;
  const prov = r.provenance;
  const strict = r.ratchet.mode === 'strict';
  const out = [];
  out.push('<!doctype html>', '<html lang="en">', '<head>', '<meta charset="utf-8">',
    '<meta name="viewport" content="width=device-width, initial-scale=1">', '<meta name="color-scheme" content="light dark">',
    `<title>${esc(`temper compliance: ${r.title} (${LABEL[r.status]}${r.raised.length ? ', baseline raised' : ''})`)}</title>`, `<style>${CSS}</style>`, '</head>', '<body>', '<main class="page">');

  out.push('<header>', '<p class="eyebrow">temper compliance report</p>', `<h1>${esc(r.title)}</h1>`,
    `<p class="meta">commit ${prov.commit ? `<code>${esc(short(prov.commit))}</code>` : 'n/a'} · toolchain ${esc(prov.toolchain ?? 'n/a')} · generated ${esc(isoMinute(prov.generated))}</p>`, '</header>');

  const jump = r.new_code.length ? ` <a href="#new-code">See the ${strict ? 'findings' : 'new-code findings'}.</a>` : '';
  out.push(`<section class="status status-${esc(r.status)}" aria-label="Status">${pill(r.status)}<p>${rich(r.status_text)}${jump}</p></section>`);
  if (r.raised.length) {
    const cap = 50;
    out.push('<section class="raised" aria-labelledby="raised">',
      `<p class="raised-head"><span id="raised" class="pill pill-raised"><span class="glyph" aria-hidden="true">▲</span>Baseline raised</span><span>${rich(raisedText(r))}</span></p>`,
      table('pairs', [['Crate'], ['Lint'], ['Before', 'num'], ['Now', 'num'], ['Change', 'num']],
        r.raised.slice(0, cap).map((x) => `<tr><td class="nowrap"><code>${esc(x.crate)}</code></td><td><code>${esc(x.lint)}</code></td>${num(x.from)}${num(x.to)}<td class="num">+${esc(fmt(x.to - x.from))}</td></tr>`)),
      r.raised.length > cap ? `<p class="more">${esc(fmt(r.raised.length - cap))} more in report.json</p>` : '', '</section>');
  }
  if (r.ratchet.notes.length) out.push(`<ul class="notes">${r.ratchet.notes.map((n) => `<li>${rich(n)}</li>`).join('')}</ul>`);

  // Headline numbers.
  const kpi = (label, value, sub, alert = false) => `<div class="kpi${alert ? ' kpi-alert' : ''}"><p class="kpi-label">${label}</p><p class="kpi-value">${value}</p><p class="kpi-sub">${sub}</p></div>`;
  const newSub = strict ? 'no baseline: none allowed' : diffed(r) ? `since <code>${esc(baseLabel(r.ratchet.base ?? 'base'))}</code>` : 'against the baseline totals';
  out.push('<section class="kpis" aria-label="Headline numbers">',
    kpi('MUST findings', esc(fmt(t.must)), `of ${esc(fmt(t.findings))} findings`),
    kpi('Iron-rule (H) findings', esc(fmt(t.h)), 'escape hatches: unwrap, panics, casts…'),
    kpi('New findings', esc(fmt(t.new)), newSub, t.new > 0),
    kpi('Fixed since baseline', esc(fmt(t.fixed)), strict ? 'no baseline' : `baseline: ${esc(fmt(t.baseline))} findings`),
    kpi('Crates clean', `${esc(fmt(cs.clean))}<small> / ${esc(fmt(cs.total))}</small>`, `${esc(fmt(cs.debt))} with debt · ${esc(fmt(cs.regressed))} regressed`),
    '</section>');

  // Severity strip (the bar is decoration; the legend carries the numbers).
  const segs = SEVERITIES.filter((s) => t[s] > 0).map((s) => `<span class="s-${s}" style="flex-grow:${Number(t[s])}"></span>`).join('');
  const legend = SEVERITIES.map((s) => `<li><span class="sw s-${s}" aria-hidden="true"></span>${s === 'unmapped' ? 'unmapped' : s.toUpperCase()} <b>${esc(fmt(t[s]))}</b></li>`).join('');
  const compare = strict
    ? `<p class="compare">No baseline (<code>${esc(BASELINE_PATH)}</code>): zero findings are allowed, so each one fails the gate.</p>`
    : `<p class="compare">Against the baseline of <b>${esc(fmt(t.baseline))}</b>${r.baseline.updated ? ` (updated ${esc(String(r.baseline.updated).slice(0, 10))})` : ''}: <b>${esc(fmt(t.legacy))}</b> legacy · <b>${esc(fmt(t.new))}</b> new · <b>${esc(fmt(t.fixed))}</b> fixed.</p>`;
  const exc = t.exceptions ? `<p class="compare">${esc(plural(t.exceptions, 'approved lint exception'))} (<code>#[expect]</code> with a reason) not counted.</p>` : '';
  out.push(`<section class="sev" aria-label="Findings by severity"><div class="sevbar" aria-hidden="true">${segs}</div><ul class="legend">${legend}</ul>${compare}${exc}</section>`);

  if (r.errors.length) {
    const errs = r.errors.slice(0, 50).map((e) => `<li>${rich(clip(errorText(e), 400))}</li>`).join('');
    out.push('<section aria-labelledby="errors"><h2 id="errors">Compile errors</h2>', '<p class="hint">These are not lints: the build fails, and clippy stops early, so the counts below are incomplete.</p>',
      `<ul class="errors">${errs}</ul>`, r.errors.length > 50 ? `<p class="more">${esc(fmt(r.errors.length - 50))} more in report.json</p>` : '', '</section>');
  }

  // Crate scorecard.
  out.push('<section aria-labelledby="crates"><h2 id="crates">Crate scorecard</h2>',
    '<p class="hint"><b>clean</b>: no MUST findings · <b>debt</b>: MUST findings, all within the baseline · <b>regressed</b>: new findings since the baseline. Lints: inherits the workspace lint table. Charter: has an <code>AGENTS.md</code>.</p>');
  const crateRows = r.crates.map((c) => '<tr>'
    + `<td class="nowrap"><code>${esc(c.name)}</code>${c.in_workspace || c.total ? '' : ' <span class="muted">(no longer in the workspace)</span>'}</td>`
    + `<td>${pill(c.status)}</td>${num(c.must)}${num(c.should)}${num(c.other)}${num(c.h)}`
    + `${num(c.new)}${strict ? '<td class="num muted">–</td>' : num(c.fixed)}`
    + `<td>${yesNo(c.lints_inherited)}</td><td>${yesNo(c.charter)}</td>`
    + `<td class="nowrap">${c.top_rules.map((x) => `${esc(x.id)} <span class="muted">${esc(fmt(x.count))}</span>`).join(', ') || '<span class="muted">–</span>'}</td></tr>`);
  out.push(crateRows.length
    ? table('scorecard', [['Crate'], ['Status'], ['MUST', 'num'], ['SHOULD', 'num'], ['Other', 'num'], ['H', 'num'], ['New', 'num'], ['Fixed', 'num'], ['Lints'], ['Charter'], ['Top rules']], crateRows)
    : '<p class="muted">No crates.</p>', '</section>');

  // Top rules.
  out.push('<section aria-labelledby="rules"><h2 id="rules">Top rules</h2>', '<p class="hint">Findings per rule; a finding that breaks two rules counts for both.</p>');
  const maxRule = Math.max(1, ...r.top_rules.map((x) => x.count));
  // The bar under each count is decoration; the number carries the value.
  const ruleRows = r.top_rules.map((x) => `<tr><td class="nowrap"><b>${esc(x.id)}</b></td><td>${x.level ? `<span class="lvl lvl-${esc(x.level)}">${esc(x.level)}</span>` : '<span class="muted">–</span>'}</td>`
    + `<td class="num count"><span class="count-in"><span class="track" aria-hidden="true"><span class="bar" style="width:${Math.round((x.count / maxRule) * 100)}%"></span></span><span class="n">${esc(fmt(x.count))}</span></span></td>`
    + `<td class="text">${x.title ? rich(x.title) : '<span class="muted">–</span>'}</td></tr>`);
  out.push(ruleRows.length ? table('rules', [['Rule'], ['Level'], ['Findings', 'num'], ['Rule text']], ruleRows) : '<p class="muted">No findings cite a rule.</p>');
  if (r.unmapped_lints.length) {
    const top = r.unmapped_lints.slice(0, 8).map((x) => `<code>${esc(x.lint)}</code> ${esc(fmt(x.count))}`).join(' · ');
    out.push(`<p class="more">Lints no rule cites (unmapped): ${top}${r.unmapped_lints.length > 8 ? ` and ${esc(fmt(r.unmapped_lints.length - 8))} more` : ''}.</p>`);
  }
  out.push('</section>');

  // Top files.
  out.push('<section aria-labelledby="files"><h2 id="files">Top files by MUST findings</h2>');
  const fileRows = r.top_files.map((f) => `<tr><td class="path"><code>${esc(f.file)}</code></td>${num(f.must)}${num(f.h)}${num(f.total)}<td class="nowrap">${esc(f.crate)}</td></tr>`);
  out.push(fileRows.length ? table('files', [['File'], ['MUST', 'num'], ['H', 'num'], ['All', 'num'], ['Crate']], fileRows) : '<p class="muted">No findings.</p>', '</section>');

  // New code and count increases.
  if (!strict || r.new_code.length) {
    out.push(`<section aria-labelledby="new-code"><h2 id="new-code">${strict ? 'Findings that fail the gate' : 'New-code findings'}</h2>`);
    if (strict) out.push('<p class="hint">No baseline, so every finding fails the gate.</p>');
    else if (diffed(r)) out.push(`<p class="hint">Findings on lines added or changed since <code>${esc(baseLabel(r.ratchet.base ?? 'the base'))}</code>. You touch it, you fix it.</p>`);
    else if (!r.ratchet.notes.length) out.push('<p class="hint">Only the totals were compared. Pass the findings file from <code>temper baseline --check --base REF --findings-out FILE</code>, or run <code>temper report --base REF</code>, to list findings on changed lines.</p>');
    else out.push('<p class="hint">Only the totals were compared (see the notes above).</p>');
    const cap = 300;
    const ncRows = r.new_code.slice(0, cap).map((f) => `<tr><td class="path"><code>${esc(loc(f))}</code></td><td class="nowrap"><code>${esc(f.lint)}</code></td>`
      + `<td class="nowrap">${esc(ruleList(f.rules))}</td><td>${f.severity === 'unmapped' ? 'unmapped' : `<span class="lvl lvl-${esc(f.severity.toUpperCase())}">${esc(f.severity.toUpperCase())}</span>`}</td><td class="msg">${rich(clip(f.message, 300))}</td></tr>`);
    if (ncRows.length) out.push(table('findings', [['Location'], ['Lint'], ['Rules'], ['Level'], ['Message']], ncRows));
    else if (diffed(r)) out.push('<p class="muted">None.</p>');
    if (r.new_code.length > cap) out.push(`<p class="more">${esc(fmt(r.new_code.length - cap))} more in report.json</p>`);
    if (r.untouched_increases.length) {
      out.push('<h2 id="increases">Counts above the baseline on untouched lines</h2>', '<p class="hint">A crate now has more findings of a lint than the baseline allows, beyond the new-code findings above: a change can make unchanged code warn (a signature change, for instance).</p>',
        table('pairs', [['Crate'], ['Lint'], ['Baseline', 'num'], ['Now', 'num'], ['Change', 'num']],
          r.untouched_increases.map((x) => `<tr><td class="nowrap"><code>${esc(x.crate)}</code></td><td><code>${esc(x.lint)}</code></td>${num(x.baseline)}${num(x.current)}<td class="num">+${esc(fmt(x.current - x.baseline))}</td></tr>`)));
    }
    if (r.fixed.length) {
      out.push('<h2 id="fixed">Fixed since the baseline</h2>', r.ratchet.ok
        ? '<p class="hint">Run <code>temper baseline --tighten</code> so these cannot come back.</p>'
        : '<p class="hint">Once the ratchet passes, <code>temper baseline --tighten</code> locks these in.</p>',
      table('pairs', [['Crate'], ['Lint'], ['Baseline', 'num'], ['Now', 'num'], ['Change', 'num']],
        r.fixed.map((x) => `<tr><td class="nowrap"><code>${esc(x.crate)}</code></td><td><code>${esc(x.lint)}</code></td>${num(x.baseline)}${num(x.current)}<td class="num">−${esc(fmt(x.baseline - x.current))}</td></tr>`)));
    }
    out.push('</section>');
  }

  const dl = [
    ['Generated', isoMinute(prov.generated)],
    ['Collected', prov.collected ? isoMinute(prov.collected) : 'n/a'],
    ['Commit', prov.commit ?? 'n/a'],
    ['Toolchain', prov.toolchain ?? 'n/a'],
    ['temper', prov.temper ?? 'n/a'],
    ['Ratchet', strict ? 'strict (no baseline)' : `${r.ratchet.mode}${prov.base ? ` against ${baseLabel(prov.base)}` : ''}, computed by ${r.ratchet.computed_by}`],
    ['Baseline', r.baseline.present ? `${fmt(t.baseline)} findings${r.baseline.commit ? ` at ${short(r.baseline.commit)}` : ''}${r.baseline.updated ? `, updated ${String(r.baseline.updated).slice(0, 10)}` : ''}` : 'none'],
    ['Source', prov.source ?? 'n/a'],
  ];
  out.push('<footer aria-label="Provenance"><p class="eyebrow">Provenance</p>', `<dl class="prov">${dl.map(([k, v]) => `<dt>${esc(k)}</dt><dd>${esc(v)}</dd>`).join('')}</dl>`, '</footer>');
  out.push('</main>', '</body>', '</html>');
  return `${out.join('\n')}\n`;
}

// ---------- CLI ----------

function parseArgs(argv) {
  const opts = { findings: null, base: null, out: null, formats: [...FORMATS], help: false, locked: false };
  const args = [...argv];
  while (args.length) {
    let a = args.shift();
    let v = null;
    const eq = a.indexOf('=');
    if (a.startsWith('--') && eq > 0) { v = a.slice(eq + 1); a = a.slice(0, eq); }
    const value = () => {
      const x = v ?? args.shift();
      if (x === undefined || x === '' || (v === null && x.startsWith('--'))) throw new Error(`${a} needs a value`);
      return x;
    };
    switch (a) {
      case '--findings': opts.findings = value(); break;
      case '--base': opts.base = value(); break;
      case '--out': opts.out = value(); break;
      case '--format': {
        opts.formats = [...new Set(value().split(',').map((s) => s.trim().toLowerCase()).filter(Boolean))];
        const bad = opts.formats.filter((f) => !FORMATS.includes(f));
        if (bad.length || !opts.formats.length) throw new Error(`unknown format ${bad.join(', ') || '(none)'}; use ${FORMATS.join(',')}`);
        break;
      }
      case '--locked': if (v !== null) throw new Error('--locked takes no value'); opts.locked = true; break;
      case '-h': case '--help': opts.help = true; break;
      default: throw new Error(`unknown argument ${a}`);
    }
  }
  return opts;
}

const isCollection = (c) => c && typeof c === 'object' && c.temper_findings === 1 && Array.isArray(c.findings);

/** The gate's complete collection for this tree (same base as the CLI's gate: TEMPER_BASE, else HEAD). */
function cachedCollection(proj) {
  const base = process.env.TEMPER_BASE || headRev(proj);
  const fp = fingerprint(proj, base);
  const c = cachedFindings(proj, fp, { base });
  return c && isComplete(c, proj.config) ? { collection: c, source: `gate cache (fingerprint ${fp})` } : null;
}

/**
 * Where the report may be written: a directory under the project's target/ or
 * .temper/ once symlinks are resolved, without `..`, and no report file a
 * symlink (safeOutputPath, the rule `baseline --findings-out` follows too), so
 * an allow-listed `report --out` cannot overwrite sources or configuration.
 * @returns {string|null} the absolute directory, or null when refused
 */
export function reportOutDir(root, out) {
  return safeOutputPath(root, out, { dir: true, files: ['report.json', 'report.md', 'report.html'] });
}

const isSymlink = (p) => { try { return fs.lstatSync(p).isSymbolicLink(); } catch { return false; } };

/**
 * `temper report`. Exit 0 when the report was written (whatever it says: the
 * gate, not the report, decides), 2 on a usage error or unusable input.
 */
export async function runReport(argv) {
  let opts;
  try { opts = parseArgs(argv); } catch (e) {
    process.stderr.write(`temper report: ${e.message}\n${USAGE}\n`);
    return 2;
  }
  if (opts.help) { process.stdout.write(`${USAGE}\n`); return 0; }
  const proj = discover(process.cwd());
  const root = proj?.root ?? process.cwd();
  const warn = (m) => process.stderr.write(`temper report: ${m}\n`);

  const outDir = reportOutDir(root, opts.out ?? path.join(root, ...DEFAULT_OUT.split('/')));
  if (!outDir) { warn(`--out ${opts.out} refused: write the report under target/ or .temper/ (no \`..\`, no symlink)`); return 2; }
  const names = { json: 'report.json', md: 'report.md', html: 'report.html' };
  const targets = opts.formats.map((f) => path.join(outDir, names[f]));
  if (targets.some(isSymlink)) { warn(`refused: a report file in ${norm(path.relative(root, outDir))} is a symlink`); return 2; }
  if (opts.base && !proj?.gitRoot) { warn('--base needs a git repository'); return 2; }
  if (opts.base && !resolveRev(proj, opts.base)) {
    warn(`--base ${opts.base} is not a commit in this repository (a typo, or a shallow clone: fetch it first)`);
    return 2;
  }

  let collection = null;
  let source = null;
  if (opts.findings) {
    const p = path.resolve(opts.findings);
    const text = readText(p);
    if (text === null) warn(`${opts.findings} not found; collecting instead`);
    else {
      try { collection = JSON.parse(text); } catch { warn(`${opts.findings} is not JSON`); return 2; }
      if (!isCollection(collection)) { warn(`${opts.findings} is not a temper findings collection (temper_findings: 1)`); return 2; }
      source = `file ${norm(path.relative(root, p)) || opts.findings}`;
    }
  }
  if (!collection && proj) {
    const hit = cachedCollection(proj);
    if (hit) ({ collection, source } = hit);
  }
  if (!collection) {
    if (!proj) { warn('not inside a Rust project (no Cargo.toml found), and no --findings file'); return 2; }
    warn('collecting findings (cargo clippy, cargo doc, scan)...');
    collection = collectFindings(proj, { cfg: proj.config, locked: opts.locked });
    source = 'collected by this run';
  }

  let baseline = null;
  try { baseline = readBaseline(root); } catch (e) { warn(`cannot read the baseline: ${e?.message ?? e}`); return 2; }
  // The ratchet is the one definition of new and legacy: the lint step's result, used as-is, or one run here.
  let res = null;
  let from = null;
  if (collection.ratchet && typeof collection.ratchet === 'object' && typeof collection.ratchet.mode === 'string') {
    res = collection.ratchet;
    from = 'baseline --check';
    if (opts.base) warn(`--base ignored: ${opts.findings} carries the ratchet result of \`baseline --check\``);
  } else {
    try { res = ratchet(collection, baseline, { proj: proj ?? undefined, base: opts.base ?? null }); } catch (e) {
      warn(`the ratchet failed: ${e?.message ?? e}`);
      return 2;
    }
    from = opts.base ? 'temper report --base' : 'temper report';
  }

  const report = buildReport(collection, baseline, res, {
    generated: new Date().toISOString(), version: temperVersion(), base: opts.base ?? undefined, source, title: path.basename(root), ratchetFrom: from,
  });
  fs.mkdirSync(outDir, { recursive: true });
  const render = { json: () => `${JSON.stringify(report, null, 2)}\n`, md: () => renderMarkdown(report), html: () => renderHtml(report) };
  for (const f of opts.formats) fs.writeFileSync(path.join(outDir, names[f]), render[f]());
  const t = report.totals;
  const vs = report.ratchet.mode === 'strict' ? '; no baseline' : `; ${fmt(t.new)} new, ${fmt(t.fixed)} fixed vs the baseline${t.raised ? `; BASELINE RAISED (${t.raised})` : ''}`;
  process.stdout.write(`temper report: ${LABEL[report.status]}: ${plural(t.findings, 'finding')} (${fmt(t.must)} MUST, ${fmt(t.h)} iron-rule)${vs}\n`
    + `wrote ${targets.map((w) => norm(path.relative(process.cwd(), w)) || w).join(', ')}\n`);
  return 0;
}
