// One reader of shell command lines for the preBash rules that look at what a command
// writes: the state-dir deny, the protected-config ask and the scan of copies and patches
// onto Rust sources. It reads quotes, escapes, expansions, heredocs, operators, subshells,
// compound-command keywords, wrappers (sudo, env, xargs, ...) and `cd`, and reports the
// files each simple command writes. It never runs anything and reads no files: a rule that
// needs the disk (a patch's text, whether a path is a directory) reads it itself.
import os from 'node:os';
import path from 'node:path';

// ---------- lexer ----------

// Outside quotes a backslash escapes the next character. Before a letter, digit, `_` or `-`
// the lexer keeps it as its own part (`bs`): bash drops it (`c\p` runs cp, `tem\per` is
// `temper`), and a PowerShell command reads it as a path separator (`.git\temper\verdicts`),
// so makeWord decides once the command is known.
const KEEP_BACKSLASH = /[\w-]/;
const REDIRECT = /^(?:&>>|&>|>>|>\||>&|>|<<<|<<-|<<|<&|<>|<)/;
const OPERATOR = /^(?:;;&|;;|;&|&&|\|\||\|&|[;&|()])/;
/** Redirection operators that write their target. */
const WRITE_REDIRECTS = new Set(['>', '>>', '>|', '&>', '&>>', '>&', '<>']);

/** Index of the `)` (or `close`) that balances the opener at `open`, skipping quoted text; s.length when unbalanced. */
function balanced(s, open, openCh = '(', closeCh = ')') {
  let depth = 0;
  for (let i = open; i < s.length; i++) {
    const c = s[i];
    if (c === '\\') { i++; continue; }
    if (c === "'") { const e = s.indexOf("'", i + 1); if (e === -1) return s.length; i = e; continue; }
    if (c === '"') {
      let j = i + 1;
      while (j < s.length && s[j] !== '"') j += s[j] === '\\' ? 2 : 1;
      i = j;
      continue;
    }
    if (c === openCh) depth++;
    else if (c === closeCh && --depth === 0) return i;
  }
  return s.length;
}

/** The expansion that starts at s[i] (`$`, a backquote): [part, index after it], or null for a lone `$`. */
function expansion(s, i, quoted) {
  if (s[i] === '`') {
    let j = i + 1;
    let src = '';
    while (j < s.length && s[j] !== '`') {
      if (s[j] === '\\' && '`$\\'.includes(s[j + 1] ?? '')) { src += s[j + 1]; j += 2; continue; }
      src += s[j++];
    }
    return [{ t: 'sub', src, s: s.slice(i, j + 1), q: quoted }, Math.min(j + 1, s.length)];
  }
  const nx = s[i + 1];
  if (nx === '(') {
    if (s[i + 2] === '(') {
      const e = balanced(s, i + 1);
      return [{ t: 'arith', s: s.slice(i, e + 1), q: quoted }, e + 1];
    }
    const e = balanced(s, i + 1);
    return [{ t: 'sub', src: s.slice(i + 2, e), s: s.slice(i, e + 1), q: quoted }, e + 1];
  }
  if (nx === '{') {
    const e = balanced(s, i + 1, '{', '}');
    const inner = s.slice(i + 2, e);
    const p = /^[A-Za-z_]\w*$/.test(inner) ? { t: 'var', name: inner } : { t: 'param' };
    return [{ ...p, s: s.slice(i, e + 1), q: quoted }, e + 1];
  }
  // PowerShell's `$env:NAME` is one variable.
  const name = /^(?:env:[A-Za-z_]\w*|[A-Za-z_]\w*|[0-9@*#?$!-])/i.exec(s.slice(i + 1, i + 256));
  if (name) return [{ t: 'var', name: name[0], s: `$${name[0]}`, q: quoted }, i + 1 + name[0].length];
  return null;
}

/**
 * The simple commands of a command line as written: `{ words, redirects, sep, after, raw }`
 * entries, with `{ open: true }` / `{ close: true }` for a subshell's parentheses. A word is
 * `{ parts }`: literal text (`lit`, with `q` when quoted), `var`, `param` (`${x:-y}`), `sub`
 * (`$(...)` or backquotes, with its `src`), `arith` and `proc` (`<(...)`). A redirect is
 * `{ op, fd, word }`; a heredoc's also carries `body` and `quoted` (its delimiter was quoted).
 * `sep` and `after` are the operators before and after the command (`|`, `&&`, `;`, newline...).
 */
function lex(src) {
  const s = String(src);
  const n = s.length;
  const out = [];
  let i = 0;
  let cmd = null;
  let word = null;
  let redir = null;
  let sep = '';
  let heredocs = [];
  const bodies = []; // [start, end) of each heredoc body
  const ensure = (at) => (cmd ??= { words: [], redirects: [], sep, after: '', start: at });
  const startWord = () => (word ??= { parts: [], start: i });
  const lit = (text, q) => {
    const w = startWord();
    const last = w.parts.at(-1);
    if (last?.t === 'lit' && last.q === q && !last.bs) last.s += text;
    else w.parts.push({ t: 'lit', s: text, q });
  };
  const endWord = () => {
    if (!word) return;
    const w = word;
    word = null;
    ensure(w.start);
    if (redir) {
      const r = { ...redir, word: w };
      redir = null;
      if (r.op === '<<' || r.op === '<<-') {
        r.delim = w.parts.filter((p) => !p.bs).map((p) => p.s).join(''); // `<<\EOF` is a quoted EOF
        r.quoted = w.parts.some((p) => p.q || p.bs);
        r.body = null;
        heredocs.push(r);
      }
      cmd.redirects.push(r);
    } else cmd.words.push(w);
  };
  const endCmd = (op, at) => {
    endWord();
    redir = null;
    if (cmd) {
      cmd.after = op;
      cmd.raw = s.slice(cmd.start, at).trim();
      out.push(cmd);
    }
    cmd = null;
    sep = op;
  };
  while (i < n) {
    const c = s[i];
    if (c === '\\') {
      if (s[i + 1] === '\n') { i += 2; continue; }
      if (s[i + 1] === '\r' && s[i + 2] === '\n') { i += 3; continue; }
      if (i + 1 < n && !KEEP_BACKSLASH.test(s[i + 1])) { lit(s[i + 1], true); i += 2; continue; }
      if (i + 1 < n) { startWord().parts.push({ t: 'lit', s: '\\', q: false, bs: true }); i++; continue; }
      lit('\\', false);
      i++;
      continue;
    }
    if (c === "'") {
      const e = s.indexOf("'", i + 1);
      const end = e === -1 ? n : e;
      lit(s.slice(i + 1, end), true);
      i = end + 1;
      continue;
    }
    if (c === '"') {
      startWord();
      lit('', true);
      let j = i + 1;
      while (j < n && s[j] !== '"') {
        if (s[j] === '\\' && j + 1 < n && '"\\$`\n'.includes(s[j + 1])) { if (s[j + 1] !== '\n') lit(s[j + 1], true); j += 2; continue; }
        if (s[j] === '$' || s[j] === '`') {
          const x = expansion(s, j, true);
          if (x) { word.parts.push(x[0]); j = x[1]; continue; }
        }
        lit(s[j], true);
        j++;
      }
      i = j + 1;
      continue;
    }
    if (c === '$' && s[i + 1] === "'") { // ANSI-C quoting
      let j = i + 2;
      let text = '';
      while (j < n && s[j] !== "'") {
        if (s[j] === '\\' && j + 1 < n) {
          const e = s[j + 1];
          text += { n: '\n', t: '\t', r: '\r', 0: '\0' }[e] ?? e;
          j += 2;
          continue;
        }
        text += s[j++];
      }
      lit(text, true);
      i = j + 1;
      continue;
    }
    if (c === '$' || c === '`') {
      const x = expansion(s, i, false);
      if (x) { startWord().parts.push(x[0]); i = x[1]; continue; }
      lit('$', false);
      i++;
      continue;
    }
    if (c === '#' && !word) {
      while (i < n && s[i] !== '\n') i++;
      continue;
    }
    if (c === '\n') {
      endCmd('\n', i);
      i++;
      for (const h of heredocs) {
        const lines = [];
        const from = i;
        for (;;) {
          if (i >= n) break;
          const e = s.indexOf('\n', i);
          const end = e === -1 ? n : e;
          const line = s.slice(i, end);
          i = end + 1;
          if ((h.op === '<<-' ? line.replace(/^\t+/, '') : line).replace(/\r$/, '') === h.delim) { h.closed = true; break; }
          lines.push(h.op === '<<-' ? line.replace(/^\t+/, '') : line);
        }
        h.body = lines.length ? `${lines.join('\n')}\n` : '';
        bodies.push([from, Math.min(i, n)]);
      }
      heredocs = [];
      continue;
    }
    if (c === ' ' || c === '\t' || c === '\r') { endWord(); i++; continue; }
    if ((c === '<' || c === '>') && s[i + 1] === '(') {
      const e = balanced(s, i + 1);
      startWord().parts.push({ t: 'proc', src: s.slice(i + 2, e), s: s.slice(i, e + 1) });
      i = e + 1;
      continue;
    }
    const r = REDIRECT.exec(s.slice(i, i + 3));
    if (r) {
      let fd = null;
      if (word && word.parts.length === 1 && word.parts[0].t === 'lit' && !word.parts[0].q && /^\d+$/.test(word.parts[0].s)) {
        fd = Number(word.parts[0].s);
        ensure(word.start);
        word = null;
      } else endWord();
      ensure(i);
      i += r[0].length;
      // `2>&1`, `>&-`, `<&3`: a descriptor, not a file.
      const dup = /^[ \t]*(?:\d+|-)(?=$|[\s;&|<>()])/.exec(s.slice(i));
      if ((r[0] === '>&' || r[0] === '<&') && dup) { i += dup[0].length; continue; }
      redir = { op: r[0], fd };
      continue;
    }
    const op = OPERATOR.exec(s.slice(i, i + 3));
    if (op) {
      if (op[0] === '(' && !cmd && !word && s[i + 1] === '(') { // `(( arithmetic ))`
        const e = balanced(s, i);
        i = e + 1;
        continue;
      }
      endCmd(op[0], i);
      i += op[0].length;
      if (op[0] === '(') out.push({ open: true });
      else if (op[0] === ')') out.push({ close: true });
      continue;
    }
    lit(c, false);
    i++;
  }
  endCmd('', n);
  out.bodies = bodies;
  return out;
}

// ---------- words ----------

/**
 * A word as the rules see it: `raw` (quotes removed, expansions as written), `text` (known
 * expansions replaced, unknown ones as written), `value` (the text, or null when any part is
 * unknown), `glob` (an unquoted `*`, `?`, `[` or brace list) and `quoted`.
 */
function makeWord(parts, env = {}) {
  let raw = '';
  let text = '';
  let known = true;
  let glob = false;
  parts.forEach((p, k) => {
    if (p.bs && !env.ps) return; // bash removes the escaping backslash
    raw += p.s;
    if (p.t === 'lit') {
      let s = p.s;
      if (!p.q && k === 0 && /^~(?=\/|\\|$)/.test(s) && env.home) s = env.home + s.slice(1);
      else if (!p.q && k === 0 && s.startsWith('~')) known = false;
      if (!p.q && (/[*?[]/.test(s) || braceList(s))) glob = true;
      text += s;
      return;
    }
    const v = partValue(p, env);
    if (v === null) { known = false; text += p.s; } else text += v;
  });
  const assign = parts[0]?.t === 'lit' && !parts[0].q ? /^[A-Za-z_]\w*(?==)/.exec(parts[0].s)?.[0] : undefined;
  return { raw, text, value: known ? text : null, glob, quoted: parts.some((p) => p.q), assign };
}

/** An unquoted brace list (`{a,b}`, `{1..3}`), found in one pass. */
function braceList(s) {
  for (const m of s.matchAll(/\{([^{}]*)\}/g)) if (m[1].includes(',') || m[1].includes('..')) return true;
  return false;
}

/** A word whose value is unknown (xargs input, find's `{}`). */
const unknownWord = (raw) => ({ raw, text: raw, value: null, glob: false, quoted: false });

/** `git rev-parse --git-dir` and friends inside `$(...)`, which name directories temper knows. */
function gitRevParse(src, env) {
  const m = /^\s*git\b(?:\s+-C\s+\S+)?\s+rev-parse\b(.*)$/s.exec(src);
  if (!m) return null;
  const args = m[1].replace(/(?:^|\s)--path-format=(?:absolute|relative)(?=\s|$)/g, ' ').trim();
  const gitPath = /--git-path\s+["']?([^\s"'`)]+)["']?\s*$/.exec(args);
  if (gitPath) return `${env.gitDir ?? '.git'}/${gitPath[1]}`;
  if (/^(?:--(?:git-common-dir|git-dir|absolute-git-dir))$/.test(args)) return env.gitDir ?? '.git';
  if (/^--show-toplevel$/.test(args)) return env.top ?? null;
  return null;
}

// Variables that name the temp directory, and a `mktemp` that makes its file or directory there.
const TEMP_VARS = /^(?:TMPDIR|TMP|TEMP|RUNNER_TEMP|env:(?:TMP|TEMP))$/i;
const MKTEMP = /^\s*mktemp((?:\s+(?:-[dqut]+|--(?:directory|quiet|dry-run|tmpdir)))*)(?:\s+([^\s/\\]+))?\s*$/;
function mktemp(src, env) {
  const m = MKTEMP.exec(src);
  // A template without -t or --tmpdir is made in the current directory.
  if (!m || (m[2] && !/(?:^|\s)(?:-[dqu]*t[dqu]*|--tmpdir)(?=\s|$)/.test(m[1]))) return null;
  return env.tmp ? path.join(env.tmp, 'tmp.XXXXXXXXXX') : null;
}

function partValue(p, env) {
  if (p.t === 'var') {
    if (env.vars?.has(p.name)) return env.vars.get(p.name);
    if (p.name === 'HOME') return env.home ?? null;
    if (p.name === 'PWD') return env.cwd ?? null;
    if (p.name === 'OLDPWD') return env.prev ?? null;
    if (TEMP_VARS.test(p.name)) return env.tmp ?? null;
    return null;
  }
  if (p.t === 'sub') return gitRevParse(p.src, env) ?? mktemp(p.src, env);
  return null;
}

/** A git-bash path (`/c/Users/...`, `/tmp/...`) as the Windows path it names; other paths unchanged. */
export function msysPath(t, platform = process.platform) {
  if (platform !== 'win32') return t;
  const drive = /^\/([A-Za-z])(\/.*)?$/.exec(t);
  if (drive) return `${drive[1].toUpperCase()}:${drive[2] ?? '/'}`;
  const tmp = /^\/tmp(\/.*)?$/.exec(t);
  if (tmp) return `${os.tmpdir().replace(/\\/g, '/')}${tmp[1] ?? ''}`;
  return t;
}

/** The absolute path `value` names from `base`, or null when it is unknown, empty, or relative to an unknown directory. */
export function resolvePath(value, base, platform = process.platform) {
  if (value === null || value === undefined || value === '') return null;
  const t = msysPath(value, platform);
  if (path.isAbsolute(t)) return path.resolve(t);
  return base ? path.resolve(base, t) : null;
}

/** The absolute path a word names when the command runs in `cwd`; null for a glob or an unknown word. */
export const wordPath = (w, cwd) => (w && !w.glob ? resolvePath(w.value, cwd) : null);

// ---------- commands ----------

const KEYWORDS = new Set(['if', 'then', 'else', 'elif', 'fi', 'do', 'done', 'while', 'until', '!', '{', '}', 'esac']);
// Wrappers that run the command after them, with their options that take a separate value.
const WRAPPERS = new Map([
  ['sudo', ['-u', '-g', '-C', '-D', '-h', '-p', '-r', '-t', '-U', '-T', '--user', '--group', '--chdir', '--prompt']],
  ['doas', ['-u', '-C']], ['env', ['-u', '-C', '-S', '--unset', '--chdir', '--split-string']], ['command', []], ['builtin', []],
  ['exec', ['-a']], ['nohup', []], ['time', ['-f', '-o', '--format', '--output']], ['nice', ['-n', '--adjustment']],
  ['ionice', ['-c', '-n', '-p']], ['stdbuf', ['-i', '-o', '-e']], ['timeout', ['-s', '-k', '--signal', '--kill-after']],
  ['xargs', ['-a', '-d', '-E', '-I', '-L', '-n', '-P', '-s', '--arg-file', '--delimiter', '--max-args', '--max-procs', '--replace']],
  ['chronic', []], ['unbuffer', []], ['setsid', []],
]);
const SHELLS = new Set(['bash', 'sh', 'zsh', 'dash', 'ksh']);
const CD = new Set(['cd', 'pushd', 'popd', 'chdir', 'set-location', 'sl']);
const DECLARE = new Set(['export', 'declare', 'local', 'readonly', 'typeset']);
const MAX_DEPTH = 4;

/** A command name as the rules compare it: basename, lower case, no `.exe`. */
const commandName = (v) => String(v ?? '').replace(/\\/g, '/').split('/').pop().toLowerCase().replace(/\.exe$/, '');

/**
 * The simple commands `cmd` runs, in order, each as `{ name, argv0, args, redirects, stdin,
 * piped, cwd, raw, line, via }`. `args` are words (see makeWord) after leading reserved
 * words (`if`, `then`, `do`, `{`, `!`, ...), `VAR=value` prefixes and wrappers (`sudo`,
 * `env`, `timeout 5`, `xargs`, ...). `cwd` is the absolute directory the command runs in,
 * following `cd`, `pushd` and `popd` (a subshell's or pipeline's `cd` ends with it), or null
 * after a `cd` temper cannot follow. Shell variables assigned earlier are expanded; `~`,
 * `$HOME`, `$PWD` and `$(git rev-parse --git-dir|--git-common-dir|--git-path p|--show-toplevel)`
 * are known. Commands inside `bash -c`, `sh -c`, `eval`, `pwsh -Command`, `find -exec`,
 * `$(...)` and a script fed to a shell on stdin are included, with `via` saying how.
 * `stdin` is `{ kind: 'file'|'heredoc'|'string'|'pipe', word?, body?, literal? }`.
 *
 * `$TMPDIR`, `$TMP`, `$TEMP`, `$env:TEMP` and `$(mktemp [-d])` name the temp directory.
 *
 * Options: `cwd` (the starting directory), `home`, `top` (`--show-toplevel`), `gitDir`, `tmp`.
 */
export function parseCommands(cmd, opts = {}) {
  const env = {
    cwd: opts.cwd ? path.resolve(opts.cwd) : null, prev: null, home: opts.home ?? os.homedir(),
    top: opts.top ?? null, gitDir: opts.gitDir ?? '.git', tmp: opts.tmp ?? os.tmpdir(), vars: new Map(), dirs: [],
  };
  const out = [];
  run(String(cmd), env, out, 0, undefined);
  return out;
}

function run(src, env, out, depth, via) {
  if (depth > MAX_DEPTH) return;
  const items = lex(src);
  // The line as code, without its heredoc bodies (a PowerShell expression is read from it).
  let line = src;
  for (const [a, b] of [...items.bodies].reverse()) line = line.slice(0, a) + line.slice(b);
  const scopes = [];
  let prevBody = null;
  for (const it of items) {
    if (it.open) { scopes.push({ cwd: env.cwd, prev: env.prev, dirs: env.dirs.length, vars: new Map(env.vars) }); continue; }
    if (it.close) {
      const o = scopes.pop();
      if (o) { env.cwd = o.cwd; env.prev = o.prev; env.dirs.length = Math.min(env.dirs.length, o.dirs); env.vars = o.vars; }
      continue;
    }
    // Command substitutions run first, in a subshell.
    for (const w of it.words.concat(it.redirects.map((r) => r.word).filter(Boolean))) {
      for (const p of w.parts) if ((p.t === 'sub' || p.t === 'proc') && p.src.trim() && !(p.t === 'sub' && gitRevParse(p.src, env) !== null)) sub(p.src, env, out, depth + 1, 'subst');
    }
    // A PowerShell command reads a kept backslash as a path separator; bash drops it (see lex).
    const ps = Boolean(env.ps) || psCommand(it.words, env);
    const wenv = ps === Boolean(env.ps) ? env : { ...env, ps };
    const piped = it.sep === '|' || it.sep === '|&';
    const isolated = piped || it.after === '|' || it.after === '|&' || it.after === '&';
    const redirects = it.redirects.map((r) => ({ op: r.op, fd: r.fd, word: r.word ? makeWord(r.word.parts, wenv) : null, body: r.body, quoted: r.quoted }));
    let stdin = piped ? { kind: 'pipe', body: prevBody } : null;
    for (const r of redirects) {
      if (r.op === '<' && r.word) stdin = { kind: 'file', word: r.word };
      else if (r.op === '<<' || r.op === '<<-') stdin = { kind: 'heredoc', body: r.body ?? null, literal: r.body !== null && r.body !== undefined && (r.quoted || !/[$`]|\\[\\$`\n]/.test(r.body)) ? r.body : null };
      else if (r.op === '<<<' && r.word) stdin = { kind: 'string', word: r.word, body: r.word.text };
    }
    prevBody = stdin && stdin.kind !== 'pipe' && stdin.kind !== 'file' ? stdin.body : null;
    const words = it.words.map((w) => makeWord(w.parts, wenv));
    simple(words, { redirects, stdin, piped, isolated, raw: it.raw, line, via }, env, out, depth);
  }
}

// PowerShell cmdlets (Verb-Noun with a common verb) and the aliases the write rules read.
const PS_NAME = /^(?:(?:get|set|new|remove|copy|move|add|clear|out|write|read|invoke|start|stop|test|import|export|select|where|foreach|rename|join|split|resolve|convert|convertto|convertfrom|format|measure|tee|push|pop|expand|compress|update|enter|sort|group|compare)-[a-z]+|cpi|mi|sc|ac|ni|sl|copy|move)$/;

/** Whether a simple command is PowerShell: a cmdlet name, `$x = ...` or a `[Type]::Method(...)` expression. */
function psCommand(words, env) {
  const ws = words.map((w) => makeWord(w.parts, { ...env, ps: false }));
  const k = ws.findIndex((w) => !w.assign);
  if (k === -1) return false;
  const first = ws[k];
  if (/^\[[\w.]+\]::/.test(first.raw) || (/^\$\w+$/.test(first.raw) && ws[k + 1]?.raw === '=')) return true;
  return first.value !== null && PS_NAME.test(commandName(first.value));
}

function sub(src, env, out, depth, via) {
  const saved = { cwd: env.cwd, prev: env.prev, dirs: env.dirs.length, vars: env.vars };
  env.vars = new Map(env.vars);
  run(src, env, out, depth, via);
  env.cwd = saved.cwd;
  env.prev = saved.prev;
  env.dirs.length = Math.min(env.dirs.length, saved.dirs);
  env.vars = saved.vars;
}

const isAssign = (w) => Boolean(w.assign);
const assignTo = (env, w) => {
  if (w.value === null) env.vars.delete(w.assign);
  else env.vars.set(w.assign, w.value.slice(w.assign.length + 1));
};

function simple(words, meta, env, out, depth) {
  let w = words;
  const emit = (name, rest, extra = {}) => out.push({
    name, argv0: rest[0], args: rest.slice(1), redirects: meta.redirects, stdin: meta.stdin, piped: meta.piped,
    cwd: extra.cwd !== undefined ? extra.cwd : env.cwd, raw: meta.raw, line: meta.line, via: meta.via,
  });
  while (w.length && KEYWORDS.has(w[0].raw) && !w[0].quoted) w = w.slice(1);
  if (!w.length) { if (meta.redirects.length) emit('', [{ raw: '', text: '', value: '' }]); return; }
  if (w[0].raw === 'function' && !w[0].quoted) { // function f { ...
    w = w.slice(2);
    while (w.length && KEYWORDS.has(w[0].raw) && !w[0].quoted) w = w.slice(1);
    if (!w.length) return;
  }
  if (['for', 'select', 'case'].includes(w[0].raw) && !w[0].quoted) {
    if (w[0].raw !== 'case' && w[1]) env.vars.delete(w[1].raw);
    return;
  }
  // Assignments: alone they set shell variables; before a command they only set its environment.
  let k = 0;
  while (k < w.length && isAssign(w[k])) k++;
  if (k === w.length) {
    if (!meta.isolated) for (const a of w) assignTo(env, a);
    if (meta.redirects.length) emit('', [{ raw: '', text: '', value: '' }]);
    return;
  }
  // PowerShell: `$x = 'value'`.
  if (w.length === 3 && /^\$\w+$/.test(w[0].raw) && w[1].raw === '=') {
    if (w[2].value === null) env.vars.delete(w[0].raw.slice(1)); else env.vars.set(w[0].raw.slice(1), w[2].value);
    return;
  }
  w = w.slice(k);
  let cwd = env.cwd;
  let xargs = null;
  for (;;) {
    if (!w.length || w[0].value === null) break;
    const name = commandName(w[0].value);
    const valued = WRAPPERS.get(name);
    if (!valued) break;
    w = w.slice(1);
    if (name === 'command' && w[0] && /^-[vV]+$/.test(w[0].raw)) return; // a lookup, not a run
    if (name === 'xargs') xargs = { replace: null };
    while (w.length && ((w[0].raw.startsWith('-') && w[0].raw !== '-') || (name === 'env' && isAssign(w[0])))) {
      const a = w[0].raw;
      w = w.slice(1);
      if (name === 'xargs') {
        const rep = /^(?:-I|--replace=?|-i)(.*)$/.exec(a);
        if (rep) {
          if (rep[1]) xargs.replace = rep[1];
          else if (a === '-I' || a === '--replace') { xargs.replace = w[0]?.raw ?? '{}'; w = w.slice(1); } else xargs.replace = '{}';
          continue;
        }
      }
      if ((name === 'env' || name === 'sudo') && (a === '-C' || a === '--chdir')) { cwd = resolvePath(w[0]?.value ?? null, cwd); w = w.slice(1); continue; }
      if (name === 'env' && /^--chdir=/.test(a)) { cwd = resolvePath(a.slice(8), cwd); continue; }
      if (valued.includes(a)) w = w.slice(1);
    }
    if (name === 'timeout' && w.length) w = w.slice(1);
    if (name === 'nice' && w[0] && /^-\d+$/.test(w[0].raw)) w = w.slice(1);
  }
  if (!w.length) { if (meta.redirects.length) emit('', [{ raw: '', text: '', value: '' }]); return; } // `exec > file`
  if (xargs) {
    if (xargs.replace) w = w.map((x) => (x.raw.includes(xargs.replace) ? unknownWord(x.raw) : x));
    else w = [...w, unknownWord('(xargs input)')];
  }
  if (w[0].value === null) { emit(null, w, { cwd }); return; }
  const name = commandName(w[0].value);
  const args = w.slice(1);

  if (CD.has(name)) {
    if (meta.isolated) return;
    const before = env.cwd;
    if (name === 'popd') env.cwd = env.dirs.length ? env.dirs.pop() : null;
    else {
      const a = args.filter((x) => x.value === null || !/^(?:--|-[LPe@]+|-path|-literalpath)$/i.test(x.raw));
      if (name === 'pushd') env.dirs.push(env.cwd);
      if (!a.length) env.cwd = name === 'pushd' ? null : env.home;
      else if (a.length > 1) env.cwd = null;
      else if (a[0].value === '-') env.cwd = env.prev;
      else env.cwd = wordPath(a[0], env.cwd);
    }
    env.prev = before;
    return;
  }
  if (DECLARE.has(name)) {
    if (!meta.isolated) for (const a of args) if (isAssign(a)) assignTo(env, a);
    return;
  }
  emit(name, w, { cwd });
  if (SHELLS.has(name)) {
    const flag = args.findIndex((a) => a.value !== null && /^-[A-Za-z]*c[A-Za-z]*$/.test(a.value));
    const script = flag !== -1 ? args[flag + 1]?.text : !args.some((a) => !a.raw.startsWith('-')) ? meta.stdin?.body : null;
    if (script) sub(script, { ...env, cwd, ps: false }, out, depth + 1, `${name} -c`);
  } else if (name === 'eval') {
    run(args.map((a) => a.text).join(' '), env, out, depth + 1, 'eval');
  } else if (name === 'pwsh' || name === 'powershell') {
    const flag = args.findIndex((a) => /^-(?:c|command)$/i.test(a.raw));
    if (flag !== -1 && args[flag + 1]) sub(args.slice(flag + 1).map((a) => a.text).join(' '), { ...env, cwd, ps: true }, out, depth + 1, `${name} -Command`);
  } else if (name === 'find' && depth < MAX_DEPTH) {
    for (let k2 = 0; k2 < args.length; k2++) {
      if (!['-exec', '-execdir', '-ok', '-okdir'].includes(args[k2].raw)) continue;
      const j = args.findIndex((a, x) => x > k2 && (a.value === ';' || a.value === '+'));
      const inner = args.slice(k2 + 1, j === -1 ? args.length : j).map((a) => (a.raw.includes('{}') ? unknownWord(a.raw) : a));
      if (inner.length) simple(inner, { ...meta, redirects: [], stdin: null, via: 'find -exec' }, { ...env, cwd: args[k2].raw.endsWith('dir') ? null : cwd, vars: new Map(env.vars), dirs: [] }, out, depth + 1);
      k2 = j === -1 ? args.length : j;
    }
  }
}

// ---------- what a command writes ----------

/** `git [-C dir] [-c k=v] <sub> args`: `{ sub, args, cwd }`, or null when the command is not git. */
function gitCommand(c) {
  if (c.name !== 'git') return null;
  let dir = c.cwd;
  const a = c.args;
  for (let i = 0; i < a.length; i++) {
    const t = a[i].raw;
    if (t === '-C') { dir = wordPath(a[++i], dir); continue; }
    if (t === '-c' || t === '--git-dir' || t === '--work-tree' || t === '--namespace') { i++; continue; }
    if (t.startsWith('-')) continue;
    return { sub: a[i].value === null ? '' : a[i].value, args: a.slice(i + 1), cwd: dir };
  }
  return { sub: '', args: [], cwd: dir };
}

const INSTALL_VALUES = ['--suffix', '--mode', '--owner', '--group', '--strip-program', '--context'];
const RSYNC_VALUES = new Set(['-e', '--rsh', '-f', '--filter', '--exclude', '--include', '--exclude-from', '--include-from', '--files-from',
  '--chmod', '--chown', '--log-file', '--backup-dir', '--suffix', '--partial-dir', '-T', '--temp-dir', '--compare-dest', '--copy-dest',
  '--link-dest', '--max-size', '--min-size', '--bwlimit', '--timeout', '--port', '-M', '--remote-option', '-B', '--block-size']);
const remote = (w) => w.value !== null && /^[^/\\]+:/.test(w.value) && !/^[A-Za-z]:(?:[\\/]|$)/.test(w.value);

/**
 * cp, mv, install, ln, git mv, rsync and scp arguments: `{ sources, dest, intoDir, recursive }`,
 * or null when nothing is copied (`install -d`, a dry run, a remote destination). `intoDir`
 * is true for `-t` or several sources, false for `-T`, and undefined when it depends on
 * whether the destination is a directory.
 */
function copyArgs(name, args) {
  const rsync = name === 'rsync' || name === 'scp';
  const valueLetters = name === 'install' ? 'tSmogZ' : rsync ? '' : 'tS';
  let target = null;
  let noTarget = false;
  let recursive = name === 'mv' || name === 'git';
  const pos = [];
  let opts = true;
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (opts && a.raw === '--') { opts = false; continue; }
    if (opts && a.raw.startsWith('--')) {
      const eq = a.raw.indexOf('=');
      const k = eq === -1 ? a.raw : a.raw.slice(0, eq);
      const v = eq === -1 ? undefined : { ...a, raw: a.raw.slice(eq + 1), text: a.text.slice(a.text.indexOf('=') + 1), value: a.value === null ? null : a.value.slice(a.value.indexOf('=') + 1) };
      if (k === '--target-directory' && !rsync) { target = v ?? args[++i] ?? null; if (!target) return null; }
      else if (k === '--no-target-directory') noTarget = true;
      else if (k === '--recursive' || k === '--archive') recursive = true;
      else if (k === '--directory' || k === '--dry-run' || k === '--list-only') return null;
      else if (v === undefined && (INSTALL_VALUES.includes(k) || (rsync && RSYNC_VALUES.has(k)))) i++;
      continue;
    }
    if (opts && /^-[^-]/.test(a.raw)) {
      if (rsync) {
        if (RSYNC_VALUES.has(a.raw)) i++;
        if (/^-[a-zA-Z]*[ra]/.test(a.raw)) recursive = true;
        if (/^-[a-zA-Z]*n/.test(a.raw)) return null; // --dry-run
        continue;
      }
      const cluster = a.raw.slice(1);
      for (let j = 0; j < cluster.length; j++) {
        const ch = cluster[j];
        if (valueLetters.includes(ch)) {
          const rest = cluster.slice(j + 1);
          const v = rest ? { raw: rest, text: rest, value: rest, glob: false } : args[++i];
          if (ch === 't') { target = v ?? null; if (!target) return null; }
          break;
        }
        if (ch === 'T') noTarget = true;
        else if ('rRa'.includes(ch)) recursive = true;
        else if (ch === 'd' && name === 'install') return null;
        else if (ch === 'n' && name === 'git') return null; // git mv -n is a dry run
      }
      continue;
    }
    pos.push(a);
  }
  let sources;
  let dest;
  let intoDir;
  if (target) { sources = pos; dest = target; intoDir = true; } else {
    if (pos.length < 2) return null;
    sources = pos.slice(0, -1);
    dest = pos.at(-1);
    intoDir = noTarget ? false : pos.length > 2 ? true : undefined;
  }
  if (rsync) {
    if (remote(dest)) return null;
    sources = sources.map((s) => (remote(s) ? unknownWord(s.raw) : s));
  }
  return { sources, dest, intoDir, recursive };
}

/** PowerShell parameters: named (`-Name value`, `-Name:value`, switches) and positional. */
function psParams(args, switches = /^(?:force|append|noclobber|nonewline|recurse|passthru|whatif|confirm|container|asbytestream)$/) {
  const named = new Map();
  const pos = [];
  for (let j = 0; j < args.length; j++) {
    const m = /^-([A-Za-z]+)(?::(.*))?$/s.exec(args[j].raw);
    if (!m || args[j].quoted) { pos.push(args[j]); continue; }
    const key = m[1].toLowerCase();
    named.set(key, m[2] !== undefined ? { raw: m[2], text: m[2], value: m[2], glob: false } : switches.test(key) ? true : args[++j]);
  }
  // A parameter may be abbreviated to any unique prefix.
  const get = (...full) => [...named].find(([k]) => k.length > 1 && full.some((f) => f.startsWith(k)))?.[1];
  return { named, pos, get };
}

/**
 * Only a bare `-WhatIf` or `-WhatIf:$true` makes a PowerShell command a dry run. Any other value
 * (`$false`, `0`, `$f`, `(1 -eq 0)`, a variable) may be false, so the command counts as running.
 */
const psDryRun = (named) => {
  const w = named.get('whatif');
  return w === true || (w !== undefined && /^\$true$/i.test(w.raw));
};

/** Copy-Item / Move-Item (and copy, move, cpi, mi): `{ sources, dest, intoDir, recursive }`, or null. */
function psCopyArgs(move, args) {
  const { pos, get, named } = psParams(args);
  if (psDryRun(named)) return null;
  const paths = [get('path'), get('literalpath'), get('pspath'), named.get('lp')].filter((x) => x && x !== true);
  if (!paths.length && pos.length) paths.push(pos.shift());
  let dest = get('destination');
  if ((!dest || dest === true) && pos.length) dest = pos.shift();
  if (!paths.length || !dest || dest === true) return null;
  const filtered = ['filter', 'include', 'exclude'].some((k) => named.has(k));
  const sources = paths.flatMap((p) => (p.value === null ? [p] : p.value.split(',').map((t) => (filtered ? unknownWord(t) : { ...p, raw: t, text: t, value: t }))));
  return { sources, dest, intoDir: sources.length > 1 ? true : undefined, recursive: move || named.has('recurse') };
}

/**
 * The value of `-o`/`--output`-style options: `-o f`, `-sSLo f`, `-of`, `-sSLof`, `--output f`,
 * `--output=f`. A short-option cluster is read letter by letter and stops at a letter in
 * `valueLetters`, whose value is the rest of the word (`-dfoo`) or the next word.
 */
function optionValues(args, short, long, valueLetters = '') {
  const out = [];
  for (let k = 0; k < args.length; k++) {
    const a = args[k].raw;
    const eq = long.find((l) => a.startsWith(`${l}=`));
    const word = (t) => ({ ...args[k], raw: t, text: t, value: args[k].value === null ? null : t });
    if (eq) out.push(word(a.slice(eq.length + 1)));
    else if (long.includes(a)) { if (args[k + 1]) out.push(args[++k]); }
    else if (short && /^-[a-zA-Z]/.test(a)) {
      for (let j = 1; j < a.length; j++) {
        const rest = a.slice(j + 1);
        if (a[j] === short) {
          if (rest) out.push(word(rest)); else if (args[k + 1]) out.push(args[++k]);
          break;
        }
        if (valueLetters.includes(a[j])) { if (!rest) k++; break; }
        if (!/[a-zA-Z]/.test(a[j])) break;
      }
    }
  }
  return out;
}
// Short options that take a value, so a cluster stops at them (`curl -dfoo`, `wget -Uo`).
const CURL_VALUES = 'AbcCdDeEFHKmoPQrtTuUwxXyYz';
const WGET_VALUES = 'aeiBtOoTwQPUlARDIX';

/** sed -i / perl -i: a short-option cluster that reaches `i` before an option taking a value (`-Mstrict` is not -i). */
const inPlace = (sed, args) => args.some((a) => (sed ? /^-[nErsuz]*[iI]/ : /^-[nplawsTtUWXc]*i/).test(a.raw) || /^--in-place(?:=|$)/.test(a.raw));

/** Files sed -i / perl -i rewrites: the operands after the script, unless -e/-f gave it. */
function inPlaceTargets(args, sed) {
  const files = [];
  let script = false;
  for (let k = 0; k < args.length; k++) {
    const a = args[k].raw;
    if (sed && (a === '-i' || a === '-I') && args[k + 1]?.raw === '' && args[k + 1].quoted) { k++; continue; } // BSD `-i ''`
    if (a === '') continue;
    if (/^-[a-zA-Z]*[ef]$/.test(a) || a === '--expression' || a === '--file') { script = true; k++; continue; }
    if (/^--(?:expression|file)=/.test(a) || /^-e./.test(a)) { script = true; continue; }
    if (a.startsWith('-')) continue;
    files.push(args[k]);
  }
  return script ? files : files.slice(1);
}

/** `git checkout <tree-ish> [--] paths` / `git restore [--source=<tree-ish>] paths`: `{ source, paths }` (source null: the index). */
function gitRestore(sub, args) {
  let source = null;
  const paths = [];
  if (sub === 'checkout') {
    const dd = args.findIndex((a) => a.raw === '--');
    const before = [];
    const head = dd === -1 ? args : args.slice(0, dd);
    for (let k = 0; k < head.length; k++) {
      if (['-b', '-B', '--orphan', '--conflict'].includes(head[k].raw)) { k++; continue; }
      if (!head[k].raw.startsWith('-')) before.push(head[k]);
    }
    source = before[0]?.raw ?? null;
    paths.push(...(dd === -1 ? before.slice(1) : args.slice(dd + 1)));
    return { source, paths };
  }
  if (args.some((a) => a.raw === '--staged' || a.raw === '-S') && !args.some((a) => a.raw === '--worktree' || a.raw === '-W')) return { source: null, paths: [] };
  for (let k = 0; k < args.length; k++) {
    const a = args[k].raw;
    if (a === '-s' || a === '--source') source = args[++k]?.raw ?? null;
    else if (a.startsWith('--source=')) source = a.slice(9);
    else if (/^-s./.test(a)) source = a.slice(2);
    else if (a !== '--' && !a.startsWith('-')) paths.push(args[k]);
  }
  return { source, paths };
}

/** A git pathspec that is a pattern, not a path: a wildcard (`*`, `?`, `[`) or magic (`:(glob)...`, `:/x`, `:!x`). */
export const pathspecPattern = (w) => /[*?[]/.test(w.raw) || /^:(?:\(|[/!^])/.test(w.raw);

// Interpreter code that writes a file: the literal (or a variable assigned one) in a write position.
const LIT_PREFIX = '[rbfuRBFU]{0,2}';
const PATH_WRAP = String.raw`(?:(?:pathlib\s*\.\s*)?Path\s*\(\s*)?`;
const ARG = String.raw`${PATH_WRAP}(?:${LIT_PREFIX}__L(?<a>\d+)__|(?<![\w.])\$?(?<v>[A-Za-z_]\w*))\s*\)?`;
// A mode: a literal, or a variable the code assigned one (`m = 'w'; open(p, m)`).
const MODE = String.raw`(?:${LIT_PREFIX}__L(?<m>\d+)__|(?<mv>[A-Za-z_]\w*)\b(?!\s*[(.\[]))`;
const CODE_WRITES = [
  [String.raw`\bopen\s*\(\s*${ARG}\s*,\s*(?:mode\s*=\s*)?${MODE}`, (m) => /[wax+]/.test(m)],
  [String.raw`\bopen\s*\(?\s*(?:my\s+)?\$?\w+\s*,\s*__L(?<m>\d+)__\s*,\s*${ARG}`, (m) => /^\s*(?:\+?>>?|\+<)/.test(m)],
  [String.raw`\bos\s*\.\s*open\s*\(\s*${ARG}\s*,[^)]*O_(?:WRONLY|RDWR|CREAT|APPEND|TRUNC)`],
  [String.raw`${ARG}\s*\.\s*(?:write_text|write_bytes|touch)\b`],
  [String.raw`${ARG}\s*\.\s*open\s*\(\s*(?:mode\s*=\s*)?${MODE}`, (m) => /[wax+]/.test(m)],
  [String.raw`\b(?:(?:write|append|output)File(?:Sync)?|writeJson(?:Sync)?|createWriteStream|truncate(?:Sync)?|(?:File|IO)\s*\.\s*(?:bin)?write|(?:Write|Append)All(?:Text|Bytes|Lines))\s*\(\s*${ARG}`],
  [String.raw`\b(?:copy(?:file|2|tree|File(?:Sync)?)?|copy_file|move|mv|rename(?:Sync)?|replace|cp(?:Sync)?|(?:sym)?link(?:Sync)?)\s*\([^,()]*(?:\([^()]*\)[^,()]*)?,\s*${ARG}`],
].map(([re, mode]) => [new RegExp(re, 'g'), mode]);

/**
 * `code` with each string literal ('...', "...", `...`, backslash escapes kept) replaced by
 * `__L<n>__`, its text pushed to `lits`. One pass: a quote that never closes stays as it is,
 * and so does every later quote of its kind.
 */
function stringLiterals(code, lits) {
  let out = '';
  const open = new Set(['"', "'", '`']);
  for (let i = 0; i < code.length;) {
    const q = code[i];
    if (!open.has(q)) { out += q; i++; continue; }
    let j = i + 1;
    while (j < code.length && code[j] !== q) j += code[j] === '\\' ? 2 : 1;
    if (j >= code.length) { open.delete(q); out += q; i++; continue; }
    out += `__L${lits.push(code.slice(i + 1, j)) - 1}__`;
    i = j + 1;
  }
  return out;
}

/**
 * Files that python, node, ruby, perl or PowerShell code writes: `open(p, 'w')`, `Path(p).write_text`,
 * `writeFileSync(p)`, `File.write(p)`, `shutil.copy(src, p)`, `fs.renameSync(src, p)`, perl's
 * `open(my $fh, '>', p)`, `[IO.File]::WriteAllText(p)`... where `p` is a string literal or a
 * variable the code assigned one. Returns the literals; reads and writes elsewhere are not listed.
 */
export function codeWriteTargets(code) {
  const lits = [];
  const text = stringLiterals(String(code), lits);
  const vars = new Map();
  for (const m of text.matchAll(new RegExp(String.raw`(?:^|[^\w$.])\$?([A-Za-z_]\w*)\s*=(?!=)\s*${PATH_WRAP}${LIT_PREFIX}__L(\d+)__`, 'g'))) vars.set(m[1], lits[Number(m[2])]);
  const out = [];
  for (const [re, mode] of CODE_WRITES) {
    for (const m of text.matchAll(re)) {
      if (mode && !mode((m.groups.mv !== undefined ? vars.get(m.groups.mv) : lits[Number(m.groups.m)]) ?? '')) continue;
      const lit = m.groups.a !== undefined ? lits[Number(m.groups.a)] : vars.get(m.groups.v);
      if (lit) out.push(lit);
    }
  }
  // Perl's two-argument open: open(FH, '>file').
  for (const m of text.matchAll(/\bopen\s*\(?\s*(?:my\s+)?\$?\w+\s*,\s*__L(\d+)__\s*\)/g)) {
    const t = /^\s*\+?>>?\s*(\S.*)$/.exec(lits[Number(m[1])]);
    if (t) out.push(t[1]);
  }
  return [...new Set(out)];
}

// A line is read once, however many PowerShell expressions it holds.
let lastLine = null;
let lastTargets = [];
function lineCodeTargets(line) {
  if (line !== lastLine) { lastTargets = codeWriteTargets(line); lastLine = line; }
  return lastTargets;
}

const INTERPRETERS = /^(?:python[\d.]*|py|node|nodejs|deno|bun|ruby|perl|php|pwsh|powershell)$/;
const PS_CONTENT = /^(?:set-content|add-content|out-file|new-item|tee-object|sc|ac|ni)$/;
const PS_COPY = /^(?:copy-item|move-item|cpi|mi|copy|move)$/;

/** The code an interpreter runs from its arguments (-c, -e, -E, -p, -r, --eval, -Command) and stdin. */
function interpreterCode(c) {
  if (!INTERPRETERS.test(c.name ?? '')) return [];
  const code = [];
  if (c.stdin?.body) code.push(c.stdin.body);
  const a = c.args;
  for (let j = 0; j < a.length; j++) {
    if ((/^-[a-zA-Z]*[ceEp]$/.test(a[j].raw) || /^(?:-r|--eval|--print|-command)$/i.test(a[j].raw)) && a[j + 1]) code.push(a[++j].text);
  }
  return code;
}

/**
 * The files one parsed command writes, in the order the shell would, as targets
 * `{ verb, raw, text, value, glob, path, cwd }` (`path` is absolute, or null when the name
 * or the directory is unknown). Copies (cp, mv, install, ln, rsync, scp, git mv, Copy-Item,
 * Move-Item) add `sources`, `intoDir` and `recursive`, and their target is the destination
 * as written (see landing). `git checkout`/`git restore` targets add `source` (the
 * revision, or null for the index). `dir: true` marks a directory written into (`tar -C`,
 * `unzip -d`, `wget -P`, `curl --output-dir`). Interpreter code targets have verb `code`.
 */
export function commandWrites(c) {
  const out = [];
  const add = (verb, w, extra = {}) => {
    if (!w || w === true) return;
    out.push({ verb, raw: w.raw, text: w.text, value: w.value, glob: w.glob, path: wordPath(w, extra.cwd !== undefined ? extra.cwd : c.cwd), cwd: c.cwd, ...extra });
  };
  for (const r of c.redirects) if (WRITE_REDIRECTS.has(r.op)) add('redirect', r.word);
  const name = c.name;
  if (!name) return out;
  const args = c.args;
  if (name === 'tee' || name === 'sponge') {
    for (const a of args) if (!a.raw.startsWith('-')) add(name, a);
  } else if (name === 'truncate') {
    for (let k = 0; k < args.length; k++) {
      if (['-s', '--size', '-r', '--reference'].includes(args[k].raw)) k++;
      else if (!args[k].raw.startsWith('-')) add(name, args[k]);
    }
  } else if ((name === 'sed' || name === 'perl') && inPlace(name === 'sed', args)) {
    for (const a of inPlaceTargets(args, name === 'sed')) add(`${name} -i`, a);
  } else if (name === 'curl') {
    for (const a of optionValues(args, 'o', ['--output'], CURL_VALUES)) if (a.raw !== '-') add(name, a);
    for (const a of optionValues(args, '', ['--output-dir'])) add(name, a, { dir: true });
  } else if (name === 'wget') {
    for (const a of optionValues(args, 'O', ['--output-document'], WGET_VALUES)) if (a.raw !== '-') add(name, a);
    for (const a of optionValues(args, 'P', ['--directory-prefix'], WGET_VALUES)) add(name, a, { dir: true });
  } else if (name === 'dd') {
    for (const a of args) if (a.raw.startsWith('of=')) add(name, { ...a, raw: a.raw.slice(3), text: a.text.slice(3), value: a.value === null ? null : a.value.slice(3) });
  } else if (name === 'tar') {
    const extract = args.some((a, j) => /^--(?:extract|get)$/.test(a.raw) || /^-[a-zA-Z]*x[a-zA-Z]*$/.test(a.raw) || (j === 0 && /^[a-zA-Z]*x[a-zA-Z]*$/.test(a.raw)));
    if (extract) for (const a of optionValues(args, 'C', ['--directory'])) add(name, a, { dir: true });
  } else if (name === 'unzip') {
    for (const a of optionValues(args, 'd', [])) add(name, a, { dir: true });
  } else if (['cp', 'mv', 'install', 'ln', 'rsync', 'scp'].includes(name)) {
    const op = copyArgs(name, args);
    if (op) add(name, op.dest, { sources: op.sources, intoDir: op.intoDir, recursive: op.recursive });
  } else if (PS_COPY.test(name)) {
    const op = psCopyArgs(/^(?:move-item|mi|move)$/.test(name), args);
    if (op) add(name, op.dest, { sources: op.sources, intoDir: op.intoDir, recursive: op.recursive });
  } else if (PS_CONTENT.test(name)) {
    const { pos, get, named } = psParams(args);
    if (!psDryRun(named)) {
      const p = get('path', 'literalpath', 'filepath');
      add(name, p && p !== true ? p : pos[0]);
    }
  } else if (name === 'git') {
    const g = gitCommand(c);
    if (g.sub === 'mv') {
      const op = copyArgs('git', g.args);
      if (op) add('git mv', op.dest, { sources: op.sources, intoDir: op.intoDir, recursive: true, cwd: g.cwd });
    } else if (g.sub === 'checkout' || g.sub === 'restore') {
      const r = gitRestore(g.sub, g.args);
      for (const p of r.paths) add(`git ${g.sub}`, p, { source: r.source, cwd: g.cwd });
    }
  }
  if (INTERPRETERS.test(name)) {
    for (const code of interpreterCode(c)) for (const lit of codeWriteTargets(code)) add('code', { raw: lit, text: lit, value: lit, glob: false });
  }
  const a0 = c.argv0?.raw ?? '';
  if (/^[[$]/.test(a0) && a0 !== '[' && a0 !== '[[') { // a PowerShell expression: [IO.File]::WriteAllText(...); `[ -f x ]` is a test
    for (const lit of lineCodeTargets(c.line)) add('code', { raw: lit, text: lit, value: lit, glob: false });
  }
  return out;
}

/** Every file `cmd` writes (see parseCommands for the options and commandWrites for the targets). */
export const writeTargets = (cmd, opts = {}) => parseCommands(cmd, opts).flatMap((c) => commandWrites(c).map((t) => ({ ...t, command: c })));

/**
 * Where one source of a copy target lands: the destination, or the source's name inside it
 * when the copy goes into a directory (`-t`, several sources, a trailing `/`, or `destIsDir`,
 * which the caller reads from the disk). rsync's `src/` copies the contents. Null when the
 * destination or the source's name is unknown.
 */
export function landing(t, src, destIsDir = false) {
  if (!t.path) return null;
  if (t.verb === 'rsync' && /[\\/]$/.test(src.raw)) return t.path;
  const into = t.intoDir ?? (destIsDir || /[\\/]$/.test(t.raw));
  if (!into) return t.path;
  if (src.value === null || src.glob) return null;
  return path.join(t.path, path.basename(src.value.replace(/[\\/]+$/, '')));
}

// ---------- patches ----------

const PATCH_VALUES = new Set(['-p', '-i', '-o', '-d', '-D', '-F', '-B', '-Y', '-z', '-V', '-r', '-g', '--strip', '--input', '--output', '--directory', '--ifdef', '--fuzz', '--prefix', '--basename-prefix', '--suffix', '--version-control', '--reject-file', '--get', '--quoting-style']);
const GIT_APPLY_VALUES = new Set(['-p', '-C', '--exclude', '--include', '--directory', '--whitespace', '--build-fake-ancestor']);

/**
 * git apply, git am and patch: `{ tool, apply, inputs, outputs, target, cwd, stored }`, or
 * null for any other command. `apply` is false for the forms that change nothing
 * (`--check`, `--stat` without `--apply`, `--dry-run`, `--help`, `--version`, `git am --abort`).
 * `inputs` are the patch files named on the command line (relative to `cwd`, which follows
 * `git -C` and `patch -d`); with none, the patch comes from `c.stdin`. `outputs` (`-o`) and
 * `target` (patch's file operand) are files it writes. `stored` marks `git am --continue`,
 * which applies patches temper cannot see.
 */
export function patchCommand(c) {
  let tool;
  let args;
  let cwd = c.cwd;
  if (c.name === 'patch') { tool = 'patch'; args = c.args; } else {
    const g = gitCommand(c);
    if (!g || (g.sub !== 'apply' && g.sub !== 'am')) return null;
    tool = `git ${g.sub}`;
    args = g.args;
    cwd = g.cwd;
  }
  let readOnly = false;
  let reportOnly = false;
  let applyToo = false;
  let stored = false;
  const inputs = [];
  const outputs = [];
  const pos = [];
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a.value === null || !a.raw.startsWith('-') || a.raw === '-') { pos.push(a); continue; }
    const eq = a.raw.indexOf('=');
    const k = eq === -1 ? a.raw : a.raw.slice(0, eq);
    if (['-h', '--help', '--version'].includes(k) || (tool === 'patch' && k === '-v')) return { tool, apply: false };
    if (tool !== 'patch') {
      if (k === '--check') readOnly = true;
      else if (['--stat', '--numstat', '--summary'].includes(k)) reportOnly = true;
      else if (k === '--apply') applyToo = true;
      else if (['--abort', '--quit', '--show-current-patch'].includes(k)) return { tool, apply: false };
      else if (['--continue', '--skip', '--resolved', '-r'].includes(k) && tool === 'git am') stored = true;
      else if (eq === -1 && GIT_APPLY_VALUES.has(k)) i++;
      continue;
    }
    if (k === '--dry-run') { readOnly = true; continue; }
    let v = eq === -1 ? undefined : { ...a, raw: a.raw.slice(eq + 1), text: a.text.slice(a.text.indexOf('=') + 1), value: a.value.slice(eq + 1) };
    const short = /^-[pioDdFBYzVrg]/.test(k) && !k.startsWith('--');
    if (short && k.length > 2) v = { raw: k.slice(2), text: k.slice(2), value: k.slice(2), glob: false };
    const key = short ? k.slice(0, 2) : k;
    if (!PATCH_VALUES.has(key)) continue;
    if (v === undefined) v = args[++i];
    if (!v) continue;
    if (key === '-i' || key === '--input') inputs.push(v);
    else if (key === '-o' || key === '--output') outputs.push(v);
    else if (key === '-d' || key === '--directory') cwd = wordPath(v, cwd);
  }
  if ((readOnly || reportOnly) && !applyToo) return { tool, apply: false };
  let target = null;
  if (tool === 'patch') {
    target = pos[0] ?? null;
    if (pos[1]) inputs.push(pos[1]);
  } else inputs.push(...pos.filter((p) => p.raw !== '-'));
  return { tool, apply: true, inputs, outputs, target, cwd, stored };
}

/**
 * Files a unified, context or git diff names, with prefixes left on. A `---`/`+++`/`***`
 * header may end in a tab or space and a timestamp (`diff -u`), and lines may end in CR.
 */
export const patchFiles = (text) => [...String(text).matchAll(/^(?:(?:\+\+\+|---|\*\*\*)[ \t]+("[^"]*"|[^\t\r\n]*?)(?:\t[^\r\n]*)?|diff --git ("[^"]*"|\S+) ("[^"]*"|\S+)|(?:rename|copy) (?:from|to) (.+?)|Index: (.+?))\r?$/gm)]
  .flatMap((m) => m.slice(1).filter(Boolean))
  .map((f) => f.replace(/^"|"$/g, '').replace(/\s+\d{4}-\d\d-\d\d[ \t].*$/, '').trim());
