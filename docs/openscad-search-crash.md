# `search()` aborts OpenSCAD when a table column isn't a string

Short version: `search("a", [[1, 2]]);` kills OpenSCAD 2026.06.12 outright — `Abort trap: 6`,
exit 134, no output at all. Upstream converts a search table's column to a string without checking
its type, and the resulting `std::bad_variant_access` reaches an uncaught terminate handler. One
line of SCAD, no diagnostic, process gone. fab answers `[]` and lives.

Found by the AO.4 heavy perf lane, which was measuring render times, not hunting crashes. Written
up here because it needs to go upstream and because the harness now has to model it: an oracle that
DIED produced no answer, so scoring it as a disagreement buries our real divergences under their
aborts (hence `openscad::Report::crashed`).

## Repro

```openscad
search("a", [[1, 2]]);
```

That is the whole file. Also reproduces with any non-string in the searched column:

```openscad
search("a", [[1, 2]]);       // abort
search("a", [[true, 2]]);    // abort
search("a", [[[1], 2]]);     // abort
search("a", [["a", 2]]);     // fine -> [0]
```

Version: OpenSCAD 2026.06.12 (macOS, arm64). Source read at `1f65580cb`.

## Cause

`builtin_search`, `src/core/builtin_functions.cc:747`:

```cpp
ft.get_utf8_char() == entryVec[index_col_num].toStrUtf8Wrapper().get_utf8_char()
```

When `findThis` is a string and `searchTable` is a vector, each row's `index_col_num` column goes
through `toStrUtf8Wrapper()` UNCONDITIONALLY. `Value` is a variant, so a column holding a number,
bool or vector throws `std::bad_variant_access` — and nothing on the path catches it.

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
sees OpenSCAD vanish with nothing pointing at the line. It's also INCONSISTENT with the rest of the
function: the neighbouring paths warn and return `[]` on a type mismatch, so this one column is the
only place a bad type is fatal rather than diagnosed.

Not memory corruption — an uncaught C++ exception, so the blast radius is denial of service, not
code execution.

## Suggested fix

Guard the column the way the function already guards its other inputs: check the type before
converting and fall through to the existing no-match path (with the usual `WARNING:`) instead of
converting blind.

## What fab does

Returns `[]` for all three crashing forms. Worth stating plainly that `[]` is a GUESS — the oracle
can't confirm it, because asking the question kills the oracle. If upstream fixes this with
different semantics (coerce the column, or skip the row), fab follows upstream.
