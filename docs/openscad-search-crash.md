# `search()` aborts OpenSCAD on a STRING key against a non-string column

Short version: `echo(search("a", [[1, 2]]));` kills OpenSCAD 2026.06.12 outright — `Abort trap: 6`,
exit 134, `libc++abi: terminating due to uncaught exception of type std::bad_variant_access`.
Upstream converts the searched column to a string without checking its type, and the resulting
`std::bad_variant_access` reaches an uncaught terminate handler. Two lines of SCAD, no diagnostic
naming the line, process gone. fab answers `[]` and lives.

Found by the AO.4 heavy perf lane, which was measuring render times, not hunting crashes. Written
up here because it needs to go upstream and because the harness now has to model it: an oracle that
DIED produced no answer, so scoring it as a disagreement buries our real divergences under their
aborts (hence `openscad::Report::crashed`).

## Repro

`search` is a FUNCTION, so it needs function position. A bare `search(...);` statement is parsed as
a MODULE call, warns `Ignoring unknown module 'search'`, and never reaches the builtin at all — the
crash does NOT reproduce that way, which is an easy false negative to hand yourself.

```openscad
echo(search("a", [[1, 2]]));
cube(1);
```

Every line below was run; the verdict is what OpenSCAD actually did:

```openscad
echo(search("a", [[1, 2]]));            // abort, rc=134
echo(search("a", [[true, 2]]));         // abort, rc=134
echo(search("a", [[[1], 2]]));          // abort, rc=134
x = search("a", [[1, 2]]);              // abort, rc=134 — not an echo() artifact
echo(search("a", [["a", 2]]));          // fine -> [0]
echo(search(1,   [[1, 2]]));            // fine -> [0]
search("a", [[1, 2]]);                  // NO crash — parsed as a module call
```

So the condition is precise: a STRING key against a column that isn't a string. A number key over
the same table is fine, which matches the source — only the string-key path converts the column.

Version: OpenSCAD 2026.06.12 (git 0a66508c, macOS arm64). Source read at `1f65580cb`.

## Cause

`builtin_search`, `src/core/builtin_functions.cc:747`:

```cpp
ft.get_utf8_char() == entryVec[index_col_num].toStrUtf8Wrapper().get_utf8_char()
```

This is inside the `findThis` is a STRING branch (line 797 dispatches there), iterating the key's
characters against a vector table. Each row's `index_col_num` column goes through
`toStrUtf8Wrapper()` UNCONDITIONALLY. `Value` is a variant, so a column holding a number, bool or
vector throws `std::bad_variant_access` — and nothing on the path catches it. The number-key branch
never converts, which is why `search(1, [[1,2]])` is fine.

Crash backtrace, trimmed:

```
libsystem_c.dylib   abort
libc++abi.dylib     __cxa_throw
OpenSCAD            std::__1::__throw_bad_variant_access[abi:ne200100]()
OpenSCAD            Value::toStrUtf8Wrapper() const
OpenSCAD            builtin_search(Arguments, Location const&)
OpenSCAD            FunctionCall::evaluate(...)
```

## Why it matters

Any shared model, library, or pasted snippet can carry it, and the failure is silent — the user
sees OpenSCAD vanish with nothing pointing at the line. `search()` over mixed-type tables is a
normal thing to write, so this is not an exotic input. It's also INCONSISTENT with the rest of the
function: the neighbouring paths warn and return `[]` on a type mismatch, so this one column is the
only place a bad type is fatal rather than diagnosed.

Not memory corruption — an uncaught C++ exception, so the blast radius is denial of service, not
code execution.

## Suggested fix

Guard the column the way the function already guards its other inputs: check the type before
converting and fall through to the existing no-match path (with the usual `WARNING:`) instead of
converting blind.

## What fab does

Returns `[]` for every crashing form. Worth stating plainly that `[]` is a GUESS — the oracle
can't confirm it, because asking the question kills the oracle. If upstream fixes this with
different semantics (coerce the column, or skip the row), fab follows upstream.
