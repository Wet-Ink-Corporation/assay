// The documented minimum versions (README.md, Install) and the warning for an older Node.
// Message only: an old Node never changes a hook decision or an exit code.
//
// temper.mjs calls earlyNodeCheck before any other code runs, so this file must still load on a
// Node older than the minimum: plain syntax, no Node 17+ APIs, only node: built-ins.
//
// Claude Code's version is not checked. Hook input carries none, and Claude Code passes hooks no
// variable that names it or its executable (CLAUDE_CODE_EXECPATH is set for the Bash tool only).
import fs from 'node:fs';
import path from 'node:path';

/** The minimums the README's Install section documents. Keep the two in step (tests/messages.test.mjs checks). */
export const MINIMUM_VERSIONS = Object.freeze({ node: '18.0.0', claudeCode: '2.1.286' });

function parts(version) {
  const m = /^v?(\d+)\.(\d+)\.(\d+)/.exec(typeof version === 'string' ? version.trim() : '');
  return m ? [Number(m[1]), Number(m[2]), Number(m[3])] : null;
}

/** True when `version` is below `minimum`. An unparsable version is unknown, never older. */
export function isOlder(version, minimum) {
  const v = parts(version);
  const m = parts(minimum);
  if (!v || !m) return false;
  for (let i = 0; i < 3; i++) if (v[i] !== m[i]) return v[i] < m[i];
  return false;
}

const nodeWarning = (v) => `temper: Node ${v} is older than the documented minimum, Node ${MINIMUM_VERSIONS.node} (README, Install). temper's hooks and CLI may fail on it and check nothing. Put Node ${MINIMUM_VERSIONS.node.split('.')[0]} or later on the PATH that Claude Code sees.`;

/** One warning line when Node is below its documented minimum. An unknown version says nothing. */
export function runtimeWarnings({ node } = {}) {
  return isOlder(node, MINIMUM_VERSIONS.node) ? [nodeWarning(node)] : [];
}

/** A cheap stand-in for discover(): is there a Cargo.toml in `dir` or above it? */
function inRustWorkspace(dir) {
  let d = path.resolve(dir);
  for (;;) {
    if (fs.existsSync(path.join(d, 'Cargo.toml'))) return true;
    const up = path.dirname(d);
    if (up === d) return false;
    d = up;
  }
}

/** The hook input's `cwd` when it names a directory that exists (as discover() takes it), else null. */
function inputCwd(raw) {
  try {
    const input = JSON.parse(raw || '{}');
    return input && typeof input.cwd === 'string' && input.cwd && fs.existsSync(input.cwd) ? input.cwd : null;
  } catch {
    return null;
  }
}

/**
 * What temper.mjs does before anything else on a Node below the minimum, or null to carry on.
 * `hook session-start` stops early only where temper cannot run (no structuredClone, Node 16 and
 * older) and only in a Rust workspace: it prints the warning for the human (systemMessage) and the
 * model (additionalContext) and exits 0. Where temper can run (Node 17), SessionStart runs as usual
 * and adds the warning itself. Other hooks carry on unchanged, so the warning comes once per
 * session. Commands warn on stderr and carry on, keeping their own output and exit code.
 * TEMPER_DISABLE=1 stays silent.
 *
 * The Rust workspace is judged where SessionStart judges it: the hook input's `cwd`, else the
 * process directory. That needs the hook input, and stdin can be read only once, so when this path
 * reads it and carries on, it returns `{ stdin }` for the hook to parse instead of reading again.
 * `runtime` (canRun, cwd, readStdin) is injectable for tests.
 */
export async function earlyNodeCheck(node, argv, env, runtime = {}) {
  if (!isOlder(node, MINIMUM_VERSIONS.node) || (env && env.TEMPER_DISABLE === '1')) return null;
  const msg = nodeWarning(node);
  if (argv[0] === 'hook') {
    if (argv[1] !== 'session-start') return null;
    const canRun = runtime.canRun !== undefined ? runtime.canRun : typeof globalThis.structuredClone === 'function';
    if (canRun) return null;
    const stdin = runtime.readStdin ? await runtime.readStdin() : '';
    if (!inRustWorkspace(inputCwd(stdin) || runtime.cwd || process.cwd())) return { stdin };
    return { stdout: JSON.stringify({ systemMessage: msg, hookSpecificOutput: { hookEventName: 'SessionStart', additionalContext: msg } }), exit: 0 };
  }
  return { stderr: `${msg}\n` };
}
