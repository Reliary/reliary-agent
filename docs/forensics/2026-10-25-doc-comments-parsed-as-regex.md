# Forensics: doc comments are parsed as regex literals

Read-only investigation, 2026-10-25. Found while trying to disprove a proposed
classifier change. No code changed by this investigation.

## Symptom

`packages/zod/src/v4/classic/schemas.ts` is 2,938 lines and 2,938 have tokens,
but declaration-shaped lines in nine TypeScript files in zod have **no rows at
all** — contiguous bands, not scattered misses:

```
  5  src/v4/mini/in-out.ts          11, 18, 23, 28, 39
  4  src/v4/core/schemas.ts         528, 538, 549, 573
  3  src/v4/classic/errors.ts       6, 9, 35
  2  src/v4/mini/deep-partial.ts    7, 64
  2  src/v4/core/memoizer.ts        323, 328
  2  packages/tsc/bench/string.ts   15, 30
  2  packages/bench/memory/indent-leak.ts  41, 50
  1  src/v4/classic/tests/redos.test.ts    8
  1  src/v4/classic/tests/error.test.ts    526
```

`errors.ts:9` is `export interface ZodError<T = unknown>` — a type that zod's
own users ask about, and it is invisible.

## Root cause: branch order in `walk_code`

```
crates/reliary-search/src/lex.rs
 496   if c == '/' && next == '/'    -> break          (line comment)
 507   if c == '/' && regex_can_start(b, i) -> scan_regex(...)
 523   if c == '/' && next == '*'    -> block_comment = true
```

`regex_can_start` returns **true at line start** and after `( = , : [ ! & | ? { ;`.
A doc comment therefore reaches the regex branch first. `scan_regex` then runs
from the leading `/`:

```
'/** See `classic/in-out.ts`. */'
 ^ scan_regex starts here
'/** See `classic/'          <- stops at the '/' in `classic/in-out.ts`
```

It stops at the first unescaped `/` inside the comment text and treats that as
the regex terminator. The remainder is now code:

```
'in-out.ts`. */'   -> one backtick
```

An **odd** number of backticks opens a template literal (`Ml::Backtick`), which
masks every following line until the next backtick or until `ML_MAX_LINES` (50)
abandons it.

The two-branch chain that makes it visible: a doc comment containing **both a
`/`** (a URL, a path, a `*/`-looking token) **and an odd number of backticks**
is parsed as a regex that swallows part of its own text.

## Blast radius, measured

| corpus | files affected | declaration-shaped lines lost |
|---|---:|---:|
| zod (TS) | 9 | 20 |
| tokio (Rust) | 0 | 0 |
| reliary (Rust) | 0 | 0 |
| sinatra (Ruby) | 0 | 0 |
| ruby | 0 | 0 |
| requests (Python) | 0 | 0 |
| haskell | 0 | 0 |
| ocaml | 0 | 0 |
| home-manager (Nix) | 0 | 0 |

The pattern is TypeScript-shaped: JSDoc with `{@link}`, `@deprecated`, paths,
and fenced code blocks. Rust tokio has no doc comment that combines the two
triggers.

Verified triggers:

```
errors.ts:5      /** @deprecated Use `z.core.$ZodIssue` from `@zod/core` ... */
                 consumed: '/** @deprecated Use `z.core.$ZodIssue` from `@zod/'
in-out.ts:10     /** See `classic/in-out.ts`. */
                 consumed: '/** See `classic/'
deep-partial.ts:5  /** Mini counterpart of `classic/deep-partial.ts`'s ... */
                 consumed: 'ini counterpart of `classic/'
indent-leak.ts:32  /** The `// Constants:` header ... what `new Function` receives. */
                 consumed: '/** The `/'
```

## Why this is the same bug class as two already fixed

| Bug | Trigger | Consequence | Status |
|---|---|---|---|
| Python stray backtick in a comment | unpaired `` ` `` | rest of file invisible | fixed |
| Ruby `''` at end of line | Nix/Ruby literal ambiguity | 1,596 of 2,174 lines blanked | fixed |
| **Doc comment parsed as regex** | `/`+odd backticks in a comment | 20 declarations in 9 TS files | **open** |

All three are one defect: **a string or regex scanner entering a state from
inside a comment**. The earlier two were fixed at the symptom, and the ordering
that generates them was never fixed.

## Fix

A `/*` at value position is always a block-comment opener: no regex literal
begins with a bare `*` (`/\*/` begins `/` then `\`). Move the `/*` branch above
the regex branch. Positional, no keyword, no new state.

## Guard that would have caught it

Every source file in a corpus must have **zero declaration-shaped lines inside
token-free bands**. That check is independent of any tool's output and does not
need an oracle. It fails on the current binary and would have failed on the two
earlier instances of the class as well.

## What this invalidates

The TypeScript 0.940 figure was measured with 9 files partly invisible, so it is
a floor, not a measurement. And a proposed "logical-line assembly" change had
been justified by TS method misses that are partly *this* bug, not multi-line
signatures: measurable multi-line interface members in zod are **2**, not the
1,073 an earlier regex suggested.
