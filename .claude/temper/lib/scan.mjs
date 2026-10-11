// Escape-hatch scanner. Pattern rules over lexer-masked Rust source.
// Severity per context: 'hard' (block), 'soft' (advise), null (ignore).
// A 'hard' hit covered by an `#[expect(<lint>, reason = "...")]` becomes an
// 'ask' hit: an exception request that a human must approve (H-05).

import {
  maskRust, lineIndex, testRegions, inRegions, countTests, countIgnored, topLevelCfgAttrArg, matchBrace, matchPair, matchAngle, splitTop,
  statementEnd, constRegions, traitImplRegions,
} from './lexer.mjs';

const REF = 'skills/rust-craft/references';

const KEYWORDS = new Set(['as', 'async', 'await', 'break', 'const', 'continue', 'dyn', 'else', 'for', 'if', 'impl', 'in', 'let', 'loop', 'match', 'move', 'mut', 'ref', 'return', 'static', 'unsafe', 'where', 'while', 'yield']);
const esc = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

// Bad calls inside a doctest (run over masked fence code).
const DOC_BAD = /\.\s*(?:unwrap\s*\(\s*\)|unwrap_err\s*\(\s*\)|expect\s*\(\s*"|expect_err\s*\(\s*")|\b(?:panic|todo|unimplemented)\s*!/;
// Fence tokens rustdoc compiles as Rust; any other token (text, toml, sh, ..) is not Rust.
const RUST_FENCE_TOKEN = /^(?:rust|no_run|should_panic|test_harness|standalone_crate|edition\d{4}|E\d{4})$/;
const NOT_COMPILED_TOKEN = /^(?:ignore(?:-.*)?|compile_fail|text)$/;

/** Is a doc-comment fence with this info string compiled as a Rust doctest? */
export function isRustFence(info) {
  const toks = String(info).split(/[\s,]+/).filter(Boolean);
  return toks.every((t) => !NOT_COMPILED_TOKEN.test(t) && RUST_FENCE_TOKEN.test(t));
}

/** Is this crate-relative path a build script? */
export const isBuildScript = (relToCrate) => relToCrate.replace(/\\/g, '/') === 'build.rs';

/**
 * Rule table. `ctx` gives severity per code context:
 *   prod = production code in a library crate, bin = production code in a binary/build script,
 *   example = examples/ and benches/ (benchmarks follow production rules),
 *   test = tests/, test modules and #[cfg(test)]/#[test] regions.
 * A rule has either `re` (optionally filtered by `keep(env, match)`) or `find(env)`,
 * which returns `{ offset, msg?, fix?, severity?, exception? }` entries.
 * A `severity` from `find` overrides the context table.
 */
export const RULES = [
  {
    key: 'unsafe', id: 'H-01', re: /\bunsafe\b/g,
    ctx: { prod: 'hard', bin: 'hard', example: 'hard', test: 'hard' },
    msg: '`unsafe` is forbidden in this workspace',
    fix: 'Find a safe API or restructure. A genuine need (FFI) is a human decision: isolate it in a -sys crate and change the lint config with approval.',
    lints: [],
  },
  {
    key: 'unwrap', id: 'H-02',
    re: /\.\s*(?:unwrap\s*\(\s*\)|unwrap_err\s*\(\s*\)|unwrap_unchecked\s*\(|expect\s*\(\s*"|expect_err\s*\(\s*")|\b(?:Option|Result)\s*::\s*(?:unwrap|expect|unwrap_unchecked|unwrap_err|expect_err)\b/g,
    ctx: { prod: 'hard', bin: 'hard', example: 'hard', test: null },
    msg: 'unwrap/expect in production code',
    fix: 'Propagate with `?`, convert with `ok_or_else`/`let ... else`, or make the invariant a type. See references/errors.md "Eliminating unwrap/expect".',
    lints: ['clippy::unwrap_used', 'clippy::expect_used'],
  },
  {
    key: 'panic', id: 'H-03', re: /\b(?:panic|todo|unimplemented|unreachable)\s*!|\bprocess\s*::\s*abort\s*\(/g,
    ctx: { prod: 'hard', bin: 'hard', example: 'hard', test: null },
    msg: 'panicking macro in production code',
    fix: 'Return an error, or restructure types so the case cannot occur (exhaustive match, newtype, NonZero, typestate). Unfinished work is `Status: WIP`, never `todo!()`.',
    lints: ['clippy::panic', 'clippy::todo', 'clippy::unimplemented', 'clippy::unreachable'],
  },
  {
    key: 'assert-in-prod', id: 'H-03', re: /\b(?:assert|assert_eq|assert_ne)\s*!/g,
    // Compile-time assertions (`const _: () = assert!(..)`, `const { assert!(..) }`) fail the build, never at run time.
    keep: (env, m) => !inRegions(env.consts, m.index),
    ctx: { prod: 'soft', bin: 'soft', example: null, test: null },
    msg: '`assert!` panics in production code',
    fix: 'Use `debug_assert!` for internal sanity checks, a compile-time `const { assert!(..) }` for constants, or return an error for conditions callers can trigger (H-03).',
    lints: [],
  },
  {
    key: 'allow-attr', id: 'H-05', re: /#\s*!?\s*\[\s*allow\s*\(/g,
    ctx: { prod: 'hard', bin: 'hard', example: 'hard', test: 'hard' },
    msg: '`#[allow]` suppresses a lint',
    fix: 'Fix the code the lint points at. If an exception is truly warranted, use `#[expect(lint, reason = "...")]`; a human will be asked to approve it.',
    lints: [],
  },
  {
    key: 'dbg', id: 'H-06', re: /\bdbg\s*!/g,
    ctx: { prod: 'hard', bin: 'hard', example: 'hard', test: 'hard' },
    msg: '`dbg!` left in code',
    fix: 'Remove it. Use `tracing::debug!` with structured fields for diagnostics that should stay.',
    lints: ['clippy::dbg_macro'],
  },
  {
    key: 'print', id: 'H-06', re: /\be?print(?:ln)?\s*!/g,
    ctx: { prod: 'hard', bin: 'hard', example: 'soft', test: null },
    msg: 'print macro in production code',
    fix: 'Libraries: use `tracing`. Binaries: write through a locked handle, `writeln!(io::stdout().lock(), ...)?`. It returns an error on a closed pipe where `println!` would panic.',
    fixBuild: 'Build scripts emit directives with `writeln!(io::stdout().lock(), "cargo::rerun-if-changed=..")?` from a `main` that returns `Result` (ARC-17, H-06).',
    lints: ['clippy::print_stdout', 'clippy::print_stderr'],
  },
  {
    key: 'leak', id: 'H-07',
    re: /\bBox\s*::\s*leak\b|\bmem\s*::\s*forget\b|\btransmute\b|\bstatic\s+mut\b/g,
    ctx: { prod: 'hard', bin: 'hard', example: 'hard', test: 'hard' },
    msg: 'ownership escape hatch (leak/forget/transmute/static mut)',
    fix: 'Model ownership explicitly: move the value to its owner, scope threads with `std::thread::scope`, or share an `Arc`. See references/ownership.md.',
    lints: ['clippy::mem_forget', 'clippy::disallowed_methods'],
  },
  {
    key: 'catch-unwind', id: 'H-03', re: /\bcatch_unwind\b/g,
    ctx: { prod: 'hard', bin: 'hard', example: 'hard', test: 'soft' },
    msg: '`catch_unwind` used as control flow',
    fix: 'Panics are bugs, not errors. Remove the panic source and return `Result`.',
    lints: ['clippy::disallowed_methods'],
  },
  {
    key: 'exit', id: 'ARC-14', re: /\bprocess\s*::\s*exit\s*\(/g,
    ctx: { prod: 'hard', bin: 'hard', example: 'hard', test: 'soft' },
    msg: '`process::exit` skips destructors and bypasses error reporting',
    fix: 'Return `ExitCode` (or `Result`) from `main` and map errors to codes at the edge.',
    lints: ['clippy::exit'],
  },
  {
    key: 'discard-ok', id: 'H-10', re: /\.\s*ok\s*\(\s*\)\s*;/g,
    ctx: { prod: 'hard', bin: 'hard', example: 'hard', test: 'soft' },
    msg: '`.ok();` silently discards an error',
    fix: 'Handle or propagate the error. If it is truly irrelevant, say why in a comment and use an explicit `if let Err(e) = ... { tracing::debug!(...) }`.',
    lints: ['clippy::unused_result_ok'],
  },
  {
    key: 'empty-err-arm', id: 'H-10', re: /\bErr\s*\(\s*(?:_\w*|\.\.)\s*\)\s*=>\s*(?:\{\s*\}|\(\s*\))/g,
    // Not silent when another arm of the same match keeps an error (keep-first-error).
    keep: (env, m) => !otherArmBindsErr(env.code, m.index),
    ctx: { prod: 'hard', bin: 'hard', example: 'hard', test: 'soft' },
    msg: 'empty `Err` match arm silently discards the error',
    fix: 'Handle or propagate the error. If it is truly irrelevant, bind and log it (`Err(e) => tracing::debug!(error = %e, "..")`) with a comment saying why (H-10, ERR-12).',
    lints: [],
  },
  {
    key: 'empty-if-let-err', id: 'H-10', find: emptyIfLetErr,
    ctx: { prod: 'hard', bin: 'hard', example: 'hard', test: 'soft' },
    msg: 'empty `if let Err(_)` block silently discards the error',
    fix: 'Handle or propagate the error. If it is truly irrelevant, bind and log it (`if let Err(e) = .. { tracing::debug!(error = %e, "..") }`) with a comment saying why (H-10, ERR-12).',
    lints: [],
  },
  {
    key: 'let-underscore', id: 'H-10', re: /\blet\s+_\s*=/g,
    ctx: { prod: 'soft', bin: 'soft', example: 'soft', test: null },
    msg: '`let _ =` may discard a failure',
    fix: 'Confirm the value is infallible or irrelevant, and leave a comment saying why. Results must be handled (ERR-12).',
    lints: [],
  },
  {
    key: 'filter-map-ok', id: 'H-10',
    re: /\.\s*filter_map\s*\(\s*(?:Result\s*::\s*ok|\|[^|]*\|[^;]*?\.\s*ok\s*\(\s*\))\s*\)/g,
    ctx: { prod: 'soft', bin: 'soft', example: 'soft', test: null },
    msg: '`filter_map(.. .ok())` silently drops every error',
    fix: 'Collect the errors (`collect::<Result<Vec<_>, _>>()?`), or count and log what is skipped, or say in a comment why dropping them is correct (H-10).',
    lints: [],
  },
  {
    key: 'parse-unwrap-or', id: 'H-10',
    re: /\.\s*parse\s*(?:::\s*<[^>]*>)?\s*\(\s*\)\s*\.\s*(?:unwrap_or|unwrap_or_else|unwrap_or_default)\s*\(/g,
    ctx: { prod: 'soft', bin: 'soft', example: 'soft', test: null },
    msg: 'parse failure replaced by a default',
    fix: 'Propagate the parse error, or log it and document why the default is the right meaning of bad input (H-10).',
    lints: [],
  },
  {
    key: 'as-cast', id: 'H-11',
    re: /\bas\s+(?:u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize|f32|f64|char)\b/g,
    ctx: { prod: 'hard', bin: 'hard', example: 'hard', test: 'soft' },
    msg: 'numeric `as` cast (can truncate, wrap or lose sign)',
    fix: 'Use `From`/`TryFrom` (`u64::from(x)`, `u32::try_from(n)?`) or `checked_*`/`saturating_*`.',
    lints: ['clippy::as_conversions', 'clippy::cast_possible_truncation', 'clippy::cast_sign_loss', 'clippy::cast_possible_wrap', 'clippy::cast_precision_loss', 'clippy::cast_lossless'],
  },
  {
    key: 'ptr-param', id: 'OWN-03', re: /:\s*&\s*(?:String|Vec\s*<|Box\s*<|PathBuf|OsString)\b/g,
    ctx: { prod: 'hard', bin: 'hard', example: 'hard', test: 'soft' },
    msg: 'borrowed owning container (`&String`, `&Vec<T>`, `&Box<T>`, `&PathBuf`)',
    fix: 'Borrow the view instead: `&str`, `&[T]`, `&T`, `&Path`, `&OsStr`.',
    lints: ['clippy::ptr_arg', 'clippy::borrowed_box'],
  },
  {
    key: 'lazy-static', id: 'MOD-03', re: /\blazy_static\s*!|\bonce_cell\s*::/g,
    ctx: { prod: 'hard', bin: 'hard', example: 'hard', test: 'hard' },
    msg: 'legacy lazy initialization crate',
    fix: 'Use `std::sync::LazyLock` / `OnceLock` (or `LazyCell` / `OnceCell`).',
    lints: [],
  },
  {
    key: 'path-attr', id: 'ARC-06', re: /#\s*\[\s*path\s*=/g,
    ctx: { prod: 'hard', bin: 'hard', example: 'hard', test: 'hard' },
    msg: '`#[path]` hides the module layout',
    fix: 'Use the `foo.rs` + `foo/bar.rs` layout.',
    lints: [],
  },
  {
    key: 'ignore-no-reason', id: 'H-12', re: /#\s*\[\s*(?:cfg_attr\s*\([^\]]*?,\s*)?ignore\s*(?=[\],)])/g,
    keep: (_env, m) => topLevelCfgAttrArg(m[0]), // not `cfg_attr(.., educe(Debug, ignore))`
    ctx: { prod: 'hard', bin: 'hard', example: 'hard', test: 'hard' },
    msg: '`#[ignore]` or `cfg_attr(.., ignore)` without a reason',
    fix: 'Do not disable tests to get green (H-12). If a test is slow, needs external resources or cannot run under Miri, write `#[ignore = "reason"]` or `#[cfg_attr(miri, ignore = "reason")]` and tell the human.',
    lints: [],
  },
  {
    key: 'async-trait', id: 'MOD-04', re: /#\s*\[\s*(?:async_trait\s*::\s*)?async_trait\b/g,
    ctx: { prod: 'soft', bin: 'soft', example: 'soft', test: 'soft' },
    msg: '`#[async_trait]` macro',
    fix: 'Use native `async fn` in traits. For `Send` futures use `fn f(&self) -> impl Future<Output = T> + Send`; for `dyn`, return a boxed future via a named alias (ASY-03).',
    lints: [],
  },
  {
    key: 'shared-mut', id: 'OWN-11', re: /\b(?:Rc|Arc)\s*<\s*(?:RefCell|Mutex|RwLock)\s*</g,
    ctx: { prod: 'soft', bin: 'soft', example: 'soft', test: null },
    msg: 'shared mutable state',
    fix: 'Justify it in a comment on the type (who shares it, why mutation must be shared). Consider single ownership, message passing, or an arena first (OWN-11).',
    lints: [],
  },
  {
    key: 'mutex-unit', id: 'OWN-12', re: /\bMutex\s*<\s*\(\s*\)\s*>/g,
    ctx: { prod: 'soft', bin: 'soft', example: 'soft', test: null },
    msg: '`Mutex<()>` guards no data',
    fix: 'Put the protected resource inside the `Mutex` (a zero-sized handle if need be) so every access needs the guard (OWN-12).',
    lints: [],
  },
  {
    key: 'global-mut', id: 'ARC-09',
    re: /\bstatic\s+[A-Z_][A-Z0-9_]*\s*:\s*[^=;]*\b(?:Mutex|RwLock|RefCell|Cell|Atomic[A-Za-z0-9]+)\b/g,
    ctx: { prod: 'soft', bin: 'soft', example: 'soft', test: 'soft' },
    msg: 'global mutable state',
    fix: 'Pass state explicitly through constructors (ARC-09). Globals couple everything and break test isolation.',
    lints: [],
  },
  {
    key: 'deref-inherit', id: 'API-19', re: /\bimpl\b[^{;]*\bDeref(?:Mut)?\s+for\b/g,
    ctx: { prod: 'soft', bin: 'soft', example: 'soft', test: null },
    msg: '`Deref` impl',
    fix: '`Deref` is for smart-pointer-like wrappers only, never to emulate inheritance on a newtype (API-19).',
    lints: [],
  },
  {
    key: 'sleep', id: 'TST-10', re: /\bthread\s*::\s*sleep\s*\(/g,
    ctx: { prod: 'soft', bin: 'soft', example: null, test: 'soft' },
    msg: '`thread::sleep`',
    fix: 'In async code this blocks the executor (ASY-06); use `tokio::time::sleep`. In tests, inject a clock or use paused time (TST-10).',
    lints: [],
  },
  {
    key: 'thread-spawn', id: 'ASY-15', re: /\bthread\s*::\s*spawn\s*\(/g,
    ctx: { prod: 'hard', bin: 'hard', example: 'hard', test: 'soft' },
    msg: '`thread::spawn` panics when the OS refuses a thread',
    fix: 'Use `thread::Builder::new().name(..).spawn(..)?` (or `spawn_scoped`), propagate the `io::Error`, and keep the `JoinHandle` (ASY-15).',
    lints: ['clippy::disallowed_methods'],
  },
  {
    key: 'detached-spawn', id: 'ASY-04',
    re: /^[ \t]*(?:(?:tokio\s*::\s*(?:task\s*::\s*)?)?spawn|(?:std\s*::\s*)?thread\s*::\s*(?:spawn|Builder\b[^;]*?\.\s*spawn))\s*\(/gm,
    // Owned when the handle is returned (tail expression) or joined/awaited in the same statement.
    keep: (env, m) => {
      const { code } = env;
      const end = statementEnd(code, m.index);
      if (code[end] !== ';') return false;
      const close = matchPair(code, m.index + m[0].length - 1, '(', ')');
      return !/^\s*\??\s*\.\s*(?:join|await)\b/.test(code.slice(close + 1, end));
    },
    ctx: { prod: 'soft', bin: 'soft', example: 'soft', test: null },
    msg: 'spawned task or thread handle not kept',
    fix: 'Own every task and thread: keep the `JoinHandle` and join it on shutdown, or spawn into a `JoinSet`/`TaskTracker`/`thread::scope` (ASY-04, H-13).',
    lints: [],
  },
  {
    key: 'unbounded', id: 'ASY-07', re: /\bunbounded(?:_channel)?\s*(?:::\s*<[^>]*>\s*)?\(/g,
    ctx: { prod: 'soft', bin: 'soft', example: 'soft', test: null },
    msg: 'unbounded channel',
    fix: 'Use a bounded channel so producers feel backpressure (ASY-07, H-13).',
    lints: [],
  },
  {
    key: 'seqcst', id: 'ASY-13', re: /\bSeqCst\b/g,
    // A comment on the same or previous line that names the order is the justification the rule asks for.
    keep: (env, m) => {
      const line = env.idx.lineOf(m.index);
      const justified = env.comments.some((c) => {
        const from = env.idx.lineOf(c.start);
        const to = env.idx.lineOf(Math.max(c.start, c.end - 1));
        return from <= line && to >= line - 1 && /\bSeqCst\b|\b(?:total|single|global)\s+order/i.test(c.text);
      });
      return !justified && !/\buse\s+[\w\s:{},*]*$/.test(env.code.slice(Math.max(0, m.index - 300), m.index));
    },
    ctx: { prod: 'soft', bin: 'soft', example: 'soft', test: null },
    msg: '`SeqCst` without a comment naming the total order it needs',
    fix: 'Say in a comment which cross-variable total order needs `SeqCst`, or use `Acquire`/`Release` and name the paired operation. Unsure means a `Mutex`, not `SeqCst` (ASY-13).',
    lints: [],
  },
  {
    key: 'fetch-update', id: 'MOD-07', re: /\.\s*fetch_update\s*\(/g,
    ctx: { prod: 'soft', bin: 'soft', example: 'soft', test: 'soft' },
    msg: '`fetch_update` is deprecated from 1.99 (renamed `try_update`)',
    fix: 'Use the dedicated read-modify-write (`fetch_add`, `fetch_max`, `swap`), or `update`/`try_update` (stable since 1.95) for a computed update (MOD-07).',
    lints: [],
  },
  {
    key: 'compiler-fence', id: 'ASY-13', re: /(?<![.\w]\s*|\bfn\s+)\bcompiler_fence\s*\(/g,
    // std's fence only: an `atomic::` path, or an import from std/core::sync::atomic. Never `fn compiler_fence`.
    keep: (env, m) => /\batomic\s*::\s*$/.test(env.code.slice(Math.max(0, m.index - 40), m.index))
      || /\buse\s+(?:std|core)\s*::\s*sync\s*::\s*atomic\s*::\s*(?:compiler_fence\b|\{[^}]*\bcompiler_fence\b)/.test(env.code),
    ctx: { prod: 'hard', bin: 'hard', example: 'hard', test: 'soft' },
    msg: '`compiler_fence` does not order memory between threads',
    fix: 'Put the ordering on the atomic operation, or use `fence(Acquire)`/`fence(Release)` paired with a named operation. `compiler_fence` is for signal handlers, which need `unsafe` (ASY-13, H-01).',
    lints: [],
  },
  {
    key: 'downcast', id: 'MOD-11', re: /\bdowncast(?:_ref|_mut)?\b|\bas_any\s*\(/g,
    ctx: { prod: 'soft', bin: 'soft', example: 'soft', test: null },
    msg: '`Any` downcasting',
    fix: 'Model the cases as an enum, or use trait methods or trait upcasting (MOD-11).',
    lints: [],
  },
  {
    key: 'unwrap-or-default', id: 'H-10',
    re: /(?<!\bparse\s*(?:::\s*<[^>]*>)?\s*\(\s*\)\s*)\.\s*unwrap_or_default\s*\(\s*\)/g,
    ctx: { prod: 'soft', bin: 'soft', example: 'soft', test: null },
    msg: '`unwrap_or_default()` may mask a failure',
    fix: 'Fine when the default is the documented meaning of absence. When it hides an error the caller should see, propagate instead (H-10).',
    lints: [],
  },
  {
    key: 'map-index', id: 'H-04', re: /(?:\b([A-Za-z_]\w*)|[)\]])\[\s*&/g,
    keep: (_env, m) => !m[1] || !KEYWORDS.has(m[1]),
    ctx: { prod: 'soft', bin: 'soft', example: 'soft', test: null },
    msg: '`map[&key]` panics when the key is missing',
    fix: 'Use `get`/`get_mut` or `entry` and handle the missing key (H-04). Clippy does not lint map or deque indexing.',
    lints: [],
  },
  {
    key: 'env-var', id: 'ARC-11', re: /\benv\s*::\s*var(?:_os)?\s*\(/g,
    ctx: { prod: 'soft', bin: null, example: null, test: 'soft' },
    msg: 'environment read inside library code',
    fix: 'Read configuration once at the composition root into a validated config type and pass it down (ARC-11).',
    lints: [],
  },
  {
    key: 'global-allocator', id: 'ARC-14', re: /#\s*\[\s*global_allocator\s*\]/g,
    // A file with a top-level `fn main` is a binary root, whatever its path.
    keep: (env) => !/^(?:pub\s+)?fn\s+main\s*\(/m.test(env.code),
    ctx: { prod: 'hard', bin: null, example: null, test: null },
    msg: '`#[global_allocator]` in a library crate',
    fix: 'Only the binary at the composition root may choose the allocator, as an H-16 decision with a benchmark (ARC-14, PERF-09). A library that sets one breaks every binary that sets another.',
    lints: [],
  },
  {
    key: 'should-panic', id: 'TST-05', re: /#\s*\[\s*should_panic\b/g,
    ctx: { prod: null, bin: null, example: null, test: 'soft' },
    msg: '`#[should_panic]` test',
    fix: 'Production code does not panic (H-03), so assert on the returned error variant instead.',
    lints: [],
  },
  {
    key: 'weak-assert', id: 'TST-05', re: /\bassert\s*!\s*\(\s*[^;]*?\.is_(?:ok|err|some|none)\s*\(\s*\)\s*\)/g,
    ctx: { prod: null, bin: null, example: null, test: 'soft' },
    msg: 'weak assertion (`is_ok()`/`is_err()` alone)',
    fix: 'Assert the exact value with `assert_eq!`, or the exact error variant with `matches!`/`assert_matches!` (TST-05).',
    lints: [],
  },
  {
    key: 'extern-crate', id: 'MOD-01', re: /\bextern\s+crate\s+(?!(?:alloc|std)\s*;|self\s+as\b)/g,
    ctx: { prod: 'soft', bin: 'soft', example: 'soft', test: 'soft' },
    msg: '`extern crate` is unnecessary in edition 2018+',
    fix: 'Remove it; use `use` paths. Only `extern crate alloc;`/`extern crate std;` in a `no_std` crate are needed (MOD-01).',
    lints: [],
  },
  {
    key: 'crate-lint-attr', id: 'MOD-09', re: /#\s*!\s*\[\s*(?:deny|warn|forbid)\s*\(/g,
    ctx: { prod: 'soft', bin: 'soft', example: 'soft', test: 'soft' },
    msg: 'crate-level lint attribute',
    fix: 'Configure lints in the workspace `[lints]` table and inherit them with `[lints] workspace = true` (MOD-09).',
    lints: [],
  },
  {
    key: 'pub-glob-reexport', id: 'API-14', re: /\bpub\s+use\s+[^;]*::\s*\*\s*;/g,
    // Globs are fine for `#[cfg]`-selected platform modules.
    keep: (env, m) => !cfgAttrAbove(env, m.index),
    ctx: { prod: 'soft', bin: null, example: null, test: null },
    msg: 'glob re-export in the public surface',
    fix: 'Re-export items by name so every public item has one deliberate path; a glob silently publishes every future item (API-14).',
    lints: [],
  },
  {
    key: 'prelude-mod', id: 'API-14', re: /\bpub\s+mod\s+prelude\b/g,
    ctx: { prod: 'soft', bin: null, example: null, test: null },
    msg: 'public `prelude` module',
    fix: 'Drop the prelude: every public item has exactly one path (API-14). If users need many imports, the module layout is wrong (ARC-05).',
    lints: [],
  },
  {
    key: 'pub-smart-ptr', id: 'API-23',
    // Only the two shapes API-23 names: shared mutable state and a runtime-chosen `dyn` implementation.
    // `Arc<str>`, `Arc<Config>` snapshots and `&Mutex<T>` parameters are idiomatic and not flagged.
    re: /\bpub\s+(?:const\s+)?(?:async\s+)?fn\s+\w+[^{;]*?\b(?:Rc|Arc)\s*<\s*(?:(?:std\s*::\s*)?(?:sync|cell)\s*::\s*)?(?:Mutex|RwLock|RefCell|dyn)\b/g,
    ctx: { prod: 'soft', bin: null, example: null, test: null },
    msg: 'public signature exposes `Arc<Mutex<..>>`/`Rc<RefCell<..>>` or `Arc<dyn ..>`',
    fix: 'Hide sharing in a handle type with a cheap `Clone` over `Arc<Inner>`, and wrap a runtime-chosen `Arc<dyn Trait>` in a named type (API-23).',
    lints: [],
  },
  {
    key: 'ops-impl', id: 'API-22',
    re: /\bimpl\b(?:\s*<[^{;]*?>)?\s+(?:(?:core|std)\s*::\s*)?(?:ops\s*::\s*)?(?:Add|Sub|Mul|Div|Rem|Neg)(?:Assign)?\b(?:\s*<[^{;]*?>)?\s+for\b/g,
    // An impl that already uses checked/saturating/wrapping arithmetic has chosen its overflow semantics.
    keep: (env, m) => {
      const open = env.code.indexOf('{', m.index);
      if (open === -1) return false;
      return !/\b(?:checked|saturating|wrapping|overflowing)_\w+\s*\(/.test(env.code.slice(open, matchBrace(env.code, open)));
    },
    ctx: { prod: 'soft', bin: 'soft', example: null, test: null },
    msg: 'arithmetic operator impl',
    fix: 'An operator cannot report overflow or an invalid result: expose `checked_*` returning `Option`/`Result`, or document wrapping or saturating semantics on the impl (API-22, ERR-09).',
    lints: [],
  },
  {
    key: 'type-alias-primitive', id: 'API-02',
    re: /^(?:pub(?:\s*\([^)]*\))?\s+)?type\s+[A-Z][A-Za-z0-9]*\s*=\s*(?:[ui](?:8|16|32|64|128|size)|f(?:32|64)|bool|char|String|&\s*'static\s+str)\s*;/gm,
    ctx: { prod: 'soft', bin: 'soft', example: null, test: null },
    msg: '`type` alias for a primitive is not a newtype',
    fix: 'Use a newtype (`pub struct UserId(u64);`) so the compiler keeps ids, units and validated values apart (API-02).',
    lints: [],
  },
  {
    key: 'get-prefix', id: 'API-01',
    re: /\bfn\s+get_(?!(?:mut|ref|pin_mut|unchecked(?:_mut)?)\b)\w+\s*(?:<[^>]*>)?\s*\(\s*(?:&\s*(?:'\w+\s+)?(?:mut\s+)?)?self\s*\)/g,
    // A trait impl cannot rename the trait's methods.
    keep: (env, m) => !inRegions(env.traitImpls, m.index),
    ctx: { prod: 'soft', bin: 'soft', example: null, test: null },
    msg: '`get_` prefix on a getter',
    fix: 'Name the getter after the field (`name()`, `name_mut()`); `get` is for keyed lookup or the single obvious value (API-01).',
    lints: [],
  },
  {
    key: 'struct-bounds', id: 'API-21', find: structBounds,
    ctx: { prod: 'soft', bin: 'soft', example: null, test: null },
    msg: 'trait bound on a generic type definition',
    fix: 'Move the bounds to the `impl` blocks that need them; every bound on the type is forced on every user and test fake (API-21). Exceptions: a field names an associated type, `?Sized`, or a `Drop` impl needs it.',
    lints: [],
  },
  {
    key: 'doc-unwrap', id: 'H-02', find: docUnwrap,
    // rustdoc runs doctests of library crates only; elsewhere the fence is advisory.
    ctx: { prod: 'hard', bin: 'soft', example: 'soft', test: 'soft' },
    msg: 'unwrap/expect/panic in a doctest',
    fix: 'Doctests are copied verbatim: use `?` with a hidden `# fn main() -> Result<(), Box<dyn std::error::Error>> {` wrapper or a `# Ok::<(), MyError>(())` tail (TST-11, H-02). Fences tagged `text`, `ignore`, `compile_fail` or `should_panic` are not checked.',
    lints: [],
  },
  {
    key: 'doc-arg-sections', id: 'API-15', find: docArgSections,
    ctx: { prod: 'soft', bin: 'soft', example: null, test: null },
    msg: '`# Arguments`/`# Returns` doc section',
    fix: 'Name parameters in the prose and document what the signature cannot say: behaviour, errors, invariants, units (API-15).',
    lints: [],
  },
  {
    key: 'expect-attr', id: 'H-05', find: expectAttrs,
    ctx: { prod: 'hard', bin: 'hard', example: 'hard', test: 'hard' },
    msg: '`#[expect]` lint exception',
    fix: 'Exception requests need human approval (H-05).',
    lints: [],
  },
  {
    // E0453 ("incompatible with previous forbid") is a hard error that `--cap-lints warn`
    // hides, so the ratchet collection checks it here. Needs the forbidden lints
    // (`scanSource(src, { forbid })`); without them it finds nothing.
    key: 'forbid-override', id: 'H-05', find: forbidOverrides,
    ctx: { prod: 'hard', bin: 'hard', example: 'hard', test: 'hard' },
    msg: '`#[allow]`/`#[expect]` of a lint the workspace forbids (E0453 without --cap-lints)',
    fix: 'Remove the attribute and fix the code: a forbidden lint admits no exception (H-05). Changing the forbid is a human decision.',
    lints: [],
  },
  {
    key: 'erased-error', id: 'H-09', find: erasedErrors,
    ctx: { prod: 'hard', bin: null, example: null, test: null },
    msg: 'public library signature returns an erased error',
    fix: 'Define a typed error enum with `thiserror` and return it (ERR-01, ERR-03).',
    lints: [],
  },
  {
    key: 'todo-comment', id: 'CRF-04', find: todoComments,
    ctx: { prod: 'soft', bin: 'soft', example: 'soft', test: 'soft' },
    msg: 'TODO without a tracking reference',
    fix: 'Link a tracking issue (`TODO(#123)`), or do it now.',
    lints: [],
  },
  {
    key: 'commented-code', id: 'H-06', find: commentedCode,
    ctx: { prod: 'soft', bin: 'soft', example: 'soft', test: 'soft' },
    msg: 'commented-out code',
    fix: 'Delete it; version control remembers.',
    lints: [],
  },
];

const PUB_SIG_BAD = [
  [/\bResult\s*<[^;{]*,\s*String\s*>/, 'Result<_, String>'],
  [/\bBox\s*<\s*dyn\s+(?:std\s*::\s*error\s*::\s*|core\s*::\s*error\s*::\s*)?Error\b/, 'Box<dyn Error>'],
  [/\banyhow\s*::\s*(?:Result|Error)\b/, 'anyhow'],
  [/\beyre\s*::\s*(?:Result|Report)\b/, 'eyre'],
];

// ------------------------------------------------------------ custom finders

/** `#[allow(..)]` / `#[expect(..)]` (inner or outer, also inside `cfg_attr`) naming a forbidden lint. */
function forbidOverrides(env) {
  if (!env.forbid?.size) return [];
  const out = [];
  const attr = /#\s*!?\s*\[/g;
  let a;
  while ((a = attr.exec(env.code))) {
    const open = env.code.indexOf('[', a.index);
    const body = env.code.slice(open + 1, matchPair(env.code, open, '[', ']'));
    const level = /\b(allow|expect)\s*\(/g;
    let m;
    while ((m = level.exec(body))) {
      const lp = m.index + m[0].length - 1;
      const inner = body.slice(lp + 1, matchPair(body, lp, '(', ')'));
      for (const item of splitTop(inner)) {
        const lint = item.replace(/\s+/g, '');
        if (/^[a-z_][a-z0-9_]*(?:::[a-z_][a-z0-9_]*)?$/.test(lint) && env.forbid.has(lint)) {
          out.push({ offset: a.index, severity: 'hard', msg: `\`#[${m[1]}(${lint})]\` overrides a lint the workspace forbids (E0453 without --cap-lints)` });
        }
      }
    }
  }
  return out;
}

function expectAttrs(env) {
  const out = [];
  const exp = /#\s*!?\s*\[\s*expect\s*\(([^\]]*)\)\s*\]/g;
  let m;
  while ((m = exp.exec(env.code))) {
    if (!/\breason\s*=/.test(m[1])) {
      out.push({ offset: m.index, severity: 'hard', msg: '`#[expect]` without a reason', fix: 'Add `reason = "..."` explaining why the lint does not apply. A human will review it.' });
    } else {
      const lint = m[1].split(',')[0].trim();
      out.push({ offset: m.index, severity: 'ask', msg: `lint exception requested for \`${lint}\``, exception: lint });
    }
  }
  return out;
}

function erasedErrors(env) {
  const out = [];
  const sig = /\bpub\s+(?:const\s+)?(?:async\s+)?fn\s+\w+[^{;]*/g;
  let m;
  while ((m = sig.exec(env.code))) {
    for (const [re, label] of PUB_SIG_BAD) {
      if (re.test(m[0])) out.push({ offset: m.index, msg: `public library signature returns ${label}` });
    }
  }
  return out;
}

function todoComments(env) {
  const out = [];
  for (const c of env.comments) {
    if (c.doc) continue;
    const body = c.text.replace(/^\/\/+|^\/\*+|\*+\/$/g, '').trim();
    if (/\b(TODO|FIXME|XXX|HACK)\b/.test(body) && !/(#\d+|https?:\/\/|\b[A-Z][A-Z0-9]+-\d+\b)/.test(body)) out.push({ offset: c.start });
  }
  return out;
}

function commentedCode(env) {
  const out = [];
  for (const c of env.comments) {
    if (c.doc) continue;
    const body = c.text.replace(/^\/\/+|^\/\*+|\*+\/$/g, '').trim();
    const startsLikeCode = /^(let|fn|use|pub|if|match|return|for|while|impl|struct|enum|mod|const|static)\b/.test(body) && /[;{}()=<>]/.test(body);
    const endsLikeCode = /[;{}]\s*$/.test(body) && /[()=:<>]|\b(let|fn|return)\b/.test(body);
    if ((startsLikeCode || endsLikeCode) && !/[.!?…]\s*$/.test(body)) out.push({ offset: c.start });
  }
  return out;
}

/** `if let Err(_) = <scrutinee> {}` with an empty body (the body brace is the first `{` at depth 0). */
function emptyIfLetErr(env) {
  const { code } = env;
  const out = [];
  const re = /\bif\s+let\s+Err\s*\(\s*(?:_\w*|\.\.)\s*\)\s*=(?!=)/g;
  let m;
  while ((m = re.exec(code))) {
    let depth = 0;
    let open = -1;
    for (let k = re.lastIndex; k < code.length; k++) {
      const ch = code[k];
      if (ch === '(' || ch === '[') depth++;
      else if (ch === ')' || ch === ']') depth--;
      else if (ch === '{' && depth === 0) { open = k; break; }
      else if (ch === '{') k = matchBrace(code, k);
      else if ((ch === ';' || ch === '}') && depth <= 0) break;
    }
    if (open === -1) continue;
    const close = matchBrace(code, open);
    if (code.slice(open + 1, close).trim() !== '') continue;
    if (/^\s*else\b/.test(code.slice(close + 1))) continue;
    out.push({ offset: m.index });
  }
  return out;
}

/** Does another arm of the `match` enclosing `offset` bind an `Err` payload to a variable? */
function otherArmBindsErr(code, offset) {
  let depth = 0;
  let open = -1;
  for (let k = offset - 1; k >= 0; k--) {
    if (code[k] === '}') depth++;
    else if (code[k] === '{') { if (depth === 0) { open = k; break; } depth--; }
  }
  if (open === -1 || !/\bmatch\b[^;{}]*$/.test(code.slice(Math.max(0, open - 300), open))) return false;
  const close = matchBrace(code, open);
  // Arms at the match's own depth: blank out nested blocks.
  let arms = '';
  for (let k = open + 1; k < close; k++) {
    if (code[k] === '{') { const e = matchBrace(code, k); arms += ' '.repeat(e - k + 1); k = e; continue; }
    arms += code[k];
  }
  return /\bErr\s*\(\s*(?:ref\s+)?(?:mut\s+)?[a-z]\w*\s*[)@]/.test(arms.replace(/\bErr\s*\(\s*_\w*\s*\)/g, ''));
}

/** Is the item at `offset` preceded by a `#[cfg(..)]` attribute? */
function cfgAttrAbove(env, offset) {
  const lines = env.code.slice(0, offset).split('\n');
  lines.pop(); // the item's own line up to the match
  for (let k = lines.length - 1; k >= 0; k--) {
    const l = lines[k].trim();
    if (!l.startsWith('#')) return false;
    if (/^#\s*\[\s*cfg\s*\(/.test(l)) return true;
  }
  return false;
}

/** Does a bound list contain a trait bound (anything but lifetimes and `?Sized`)? */
function traitBound(bounds) {
  return splitTop(bounds, '+').map((t) => t.trim()).some((t) => t && !t.startsWith("'") && t !== '?Sized');
}

function structBounds(env) {
  const { code } = env;
  const out = [];
  const re = /\b(?:struct|enum)\s+([A-Za-z_]\w*)\s*</g;
  let m;
  while ((m = re.exec(code))) {
    const open = m.index + m[0].length - 1;
    const close = matchAngle(code, open);
    if (close < 0) continue;
    const bounded = new Set();
    for (const p of splitTop(code.slice(open + 1, close))) {
      const t = p.trim();
      if (!t || t.startsWith("'") || /^const\b/.test(t)) continue;
      const mm = /^([A-Za-z_]\w*)\s*:([^=]*)/.exec(t);
      if (mm && traitBound(mm[2])) bounded.add(mm[1]);
    }
    let k = close + 1;
    while (/\s/.test(code[k] ?? '')) k++;
    let body = '';
    if (code[k] === '(') {
      const pc = matchPair(code, k, '(', ')');
      body = code.slice(k, pc + 1);
      k = pc + 1;
    }
    const w = /^\s*where\b([^{;]*)/.exec(code.slice(k));
    if (w) {
      k += w[0].length;
      for (const pred of splitTop(w[1])) {
        const mm = /^\s*([^:]+?)\s*:([\s\S]*)$/.exec(pred);
        if (mm && !mm[1].trim().startsWith("'") && traitBound(mm[2])) bounded.add(mm[1].trim());
      }
    }
    if (!bounded.size) continue;
    if (!body) {
      while (/\s/.test(code[k] ?? '')) k++;
      if (code[k] === '{') body = code.slice(k, matchBrace(code, k) + 1);
    }
    const assoc = [...bounded].some((n) => new RegExp(`(?:^|[^\\w:])${esc(n)}\\s*::|<\\s*${esc(n)}\\s+as\\b`).test(body));
    const drop = new RegExp(`\\bimpl\\b[^{;]*\\bDrop\\s+for\\s+${esc(m[1])}\\b`).test(code);
    if (!assoc && !drop) out.push({ offset: m.index });
  }
  return out;
}

/**
 * Doc comments (line and block forms, outer and inner) as runs of consecutive
 * lines, split into fenced code blocks and prose lines.
 */
function docBlocks(env) {
  const runs = [];
  let run = null;
  let prevEnd = -1;
  let prevInner = null;
  for (const c of env.comments) {
    if (!c.doc) continue;
    const inner = /^\/[/*]!/.test(c.text);
    if (!(run && inner === prevInner && env.code.slice(prevEnd, c.start).trim() === '')) {
      run = [];
      runs.push(run);
    }
    prevEnd = c.end;
    prevInner = inner;
    if (c.text.startsWith('//')) {
      const p = /^\/\/[/!] ?/.exec(c.text)[0].length;
      run.push({ text: c.text.slice(p).replace(/\r$/, ''), offset: c.start + p });
    } else {
      const bodyEnd = c.text.endsWith('*/') ? c.text.length - 2 : c.text.length;
      let pos = 3;
      for (const line of c.text.slice(3, bodyEnd).split('\n')) {
        const lead = /^[ \t]*(?:\* ?)?/.exec(line)[0].length;
        run.push({ text: line.slice(lead).replace(/\r$/, ''), offset: c.start + pos + lead });
        pos += line.length + 1;
      }
    }
  }
  const fences = [];
  const prose = [];
  for (const lines of runs) {
    let fence = null;
    for (const l of lines) {
      const f = /^ {0,3}(`{3,}|~{3,})\s*([^`]*?)\s*$/.exec(l.text);
      if (!fence) {
        if (f) fence = { marker: f[1], info: f[2], lines: [] };
        else prose.push(l);
      } else if (f && f[1][0] === fence.marker[0] && f[1].length >= fence.marker.length && f[2] === '') {
        fences.push(fence);
        fence = null;
      } else {
        fence.lines.push(l);
      }
    }
    if (fence) fences.push(fence); // rustdoc closes an unterminated fence at the end of the docs
  }
  return { fences, prose };
}

function docUnwrap(env) {
  const out = [];
  for (const f of env.doc.fences) {
    if (!isRustFence(f.info) || /(?:^|[\s,])should_panic(?:$|[\s,])/.test(f.info)) continue;
    // Hidden lines (`# ..`) are compiled too, so they are checked.
    const body = f.lines.map((l) => l.text.replace(/^(\s*)# /, '$1').replace(/^(\s*)#$/, '$1')).join('\n');
    maskRust(body).code.split('\n').forEach((line, i) => {
      if (DOC_BAD.test(line)) out.push({ offset: f.lines[i].offset });
    });
  }
  return out;
}

function docArgSections(env) {
  return env.doc.prose
    .filter((l) => /^\s*(?:#{1,6}\s*)?(?:\*\*)?(?:Arguments|Parameters|Params|Returns|Return value)(?:\*\*)?\s*:?\s*(?:\*\*)?\s*$/.test(l.text))
    .map((l) => ({ offset: l.offset }));
}

// ------------------------------------------------------------------ scanning

/** Classify a file path relative to its crate root. */
export function classifyPath(relToCrate, crateHasLib) {
  const p = relToCrate.replace(/\\/g, '/');
  if (/^tests\//.test(p)) return 'test';
  if (/^examples\//.test(p) || /^benches\//.test(p)) return 'example';
  if (p === 'build.rs' || p === 'src/main.rs' || /^src\/bin\//.test(p)) return 'bin';
  if (/(^|\/)tests?\.rs$/.test(p) || /\/tests\//.test(p)) return 'test';
  if (/^src\//.test(p) && !crateHasLib) return 'bin';
  return 'prod';
}

function contextAt(kind, regions, offset) {
  if (kind === 'test') return 'test';
  if (inRegions(regions, offset)) return 'test';
  return kind; // prod | bin | example
}

/** Is there an #[expect(<one of lints>, reason = ...)] in the preceding lines? */
function exceptionFor(code, idx, offset, lints) {
  if (!lints.length) return null;
  const line = idx.lineOf(offset);
  const fromLine = Math.max(1, line - 12);
  const from = idx.starts[fromLine - 1];
  const window = code.slice(from, offset);
  const re = /#\s*!?\s*\[\s*expect\s*\(([^\]]*)\)\s*\]/g;
  let m;
  let found = null;
  while ((m = re.exec(window))) {
    const inner = m[1];
    if (!/\breason\s*=/.test(inner)) continue;
    const hit = lints.find((l) => inner.replace(/\s+/g, '').includes(l));
    if (hit) found = hit;
  }
  return found;
}

function regexMatches(rule, env) {
  const out = [];
  const re = new RegExp(rule.re.source, rule.re.flags);
  let m;
  while ((m = re.exec(env.code))) {
    if (m[0] === '') { re.lastIndex++; continue; }
    if (rule.keep && !rule.keep(env, m)) continue;
    out.push({ offset: m.index });
  }
  return out;
}

function makeEnv(src, kind, build, forbid) {
  const { code, comments } = maskRust(src);
  const idx = lineIndex(code);
  const env = { src, code, comments, idx, kind, build, forbid, regions: testRegions(code) };
  const lazy = (name, fn) => Object.defineProperty(env, name, { get() { const v = fn(); Object.defineProperty(env, name, { value: v }); return v; }, configurable: true });
  lazy('consts', () => constRegions(code));
  lazy('traitImpls', () => traitImplRegions(code));
  lazy('doc', () => docBlocks(env));
  return env;
}

/**
 * Scan one Rust source.
 * @param {string} src
 * @param {{kind: 'prod'|'bin'|'example'|'test', build?: boolean, forbid?: Set<string>}} opts
 *   `build`: the file is a build script (only changes fix texts).
 *   `forbid`: lints the crate forbids (`unsafe_code`, `clippy::x`), for the forbid-override rule.
 */
export function scanSource(src, { kind, build = false, forbid = null }) {
  const env = makeEnv(src, kind, build, forbid);
  const { code, idx, regions } = env;
  const hits = [];
  const push = (rule, offset, severity, extra = {}) => {
    const line = idx.lineOf(offset);
    hits.push({
      key: rule.key, id: rule.id, severity, line,
      text: idx.lineText(line, src).trim().slice(0, 160),
      ...extra,
      msg: extra.msg ?? rule.msg,
      fix: extra.fix ?? ((build && rule.fixBuild) || rule.fix),
    });
  };
  for (const rule of RULES) {
    for (const f of rule.find ? rule.find(env) : regexMatches(rule, env)) {
      const { offset, severity: forced, ...extra } = f;
      const sev = forced ?? rule.ctx[contextAt(kind, regions, offset)];
      if (!sev) continue;
      if (sev === 'hard' && !forced) {
        const lint = exceptionFor(code, idx, offset, rule.lints);
        if (lint) { push(rule, offset, 'ask', { ...extra, exception: lint }); continue; }
      }
      push(rule, offset, sev, extra);
    }
  }
  return { hits, tests: countTests(code), ignores: countIgnored(code), clones: (code.match(/\.\s*clone\s*\(\s*\)/g) ?? []).length };
}

/** Hits present in `after` but not in `before` (matched by rule key + normalized line text). */
export function newHits(beforeHits, afterHits) {
  const pool = new Map();
  for (const h of beforeHits) {
    const k = `${h.key}\u0000${h.text}`;
    pool.set(k, (pool.get(k) ?? 0) + 1);
  }
  const fresh = [];
  for (const h of afterHits) {
    const k = `${h.key}\u0000${h.text}`;
    const n = pool.get(k) ?? 0;
    if (n > 0) pool.set(k, n - 1); else fresh.push(h);
  }
  return fresh;
}

export function formatHit(h, file) {
  const where = file ? `${file}:${h.line}` : `line ${h.line}`;
  return `${h.id} ${where}: ${h.msg}. \`${h.text}\`\n    fix: ${h.fix}`;
}

export { REF };
