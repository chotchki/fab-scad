# `search()` throws `bad_variant_access` — the GUI catches it, the CLI aborts

Short version: TWO bugs, and the second is the bigger one.

1. `search()` converts the searched column to a string without checking its type, so a string key
   against a non-string column throws `std::bad_variant_access`.
2. Nothing on the COMMAND-LINE path catches it. `echo(search("a", [[1, 2]]));` gives `Abort trap: 6`,
   exit 134, `libc++abi: terminating due to uncaught exception`. The same file in the GUI prints
   `ERROR: Compilation aborted by exception: bad_variant_access` and carries on.

So the throw is a `search()` bug; the ABORT is a CLI bug, and it generalises past `search()` — any
uncaught exception out of evaluation kills a headless run with no diagnostic, while the GUI degrades
gracefully. Batch and CI usage is systematically less protected than interactive usage.

fab answers `[]` and lives.

Found by the AO.4 heavy perf lane, which was measuring render times, not hunting crashes. The
GUI-vs-CLI split is chotchki's — pasting the repro into the GUI is what showed the abort is not
inherent to the exception. Written up here because it needs to go upstream and because the harness
now has to model it: an oracle that DIED produced no answer, so scoring it as a disagreement buries
our real divergences under their aborts (hence `openscad::Report::crashed`). Note the harness only
ever sees this BECAUSE it runs headless.

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

### The command line, copy-pasteable

```console
$ printf 'echo(search("a", [[1, 2]]));\n' > crash.scad

$ openscad -o out.stl crash.scad; echo "exit=$?"
libc++abi: terminating due to uncaught exception of type std::bad_variant_access: bad_variant_access
exit=134
```

Exit 134 is 128+6, i.e. SIGABRT. Note what is NOT there: no `ERROR:`, no file, no line number,
nothing a build log could act on. `--export-format echo` aborts identically, so it is the
evaluation that dies, not the exporter.

Open the SAME file in the GUI and it prints, in the console, and keeps running:

```
ERROR: Compilation aborted by exception: bad_variant_access
```

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

## Why the GUI survives

`src/gui/MainWindow.cc:773` wraps the compile in

```cpp
} catch (const HardWarningException&) { exceptionCleanup();
} catch (const std::exception& ex)    { UnknownExceptionCleanup(ex.what());
} catch (...)                         { UnknownExceptionCleanup(); }
```

`UnknownExceptionCleanup` (MainWindow.cc:2356) logs `Compilation aborted by exception: %1$s` — which
is exactly the console line the GUI shows. `parseDocument` gets the same treatment at :3141.

`src/openscad.cc` has nine catch blocks and NOT ONE of them is around evaluation — its
`catch (const std::exception&)` sits on `po::store(...)`, commented "Catches e.g. unknown options".
The headless render path runs bare.

## Why it matters

Any shared model, library, or pasted snippet can carry it, and headless the failure is silent — the
process vanishes with nothing pointing at the line. `search()` over mixed-type tables is a normal
thing to write, so this is not an exotic input. It's also INCONSISTENT with the rest of the
function: the neighbouring paths warn and return `[]` on a type mismatch, so this one column is the
only place a bad type is fatal rather than diagnosed.

The CLI half is worse than this one builtin. Whatever OTHER unguarded variant accesses exist in the
evaluator, they are all clean errors in the GUI and hard aborts in `openscad -o`. That asymmetry is
invisible to anyone testing interactively.

Not memory corruption — an uncaught C++ exception, so the blast radius is denial of service, not
code execution.

## Suggested fix

Two, independently useful:

1. **`builtin_search`** — guard the column the way the function already guards its other inputs:
   check the type before converting and fall through to the existing no-match path (with the usual
   `WARNING:`) instead of converting blind.
2. **`openscad.cc`** — give the headless path the net the GUI already has, so an escaped exception
   is a diagnosed `ERROR:` and a nonzero exit rather than `abort()`. This is the fix that covers the
   next one of these, whatever it turns out to be.

## What fab does

Returns `[]` for every crashing form. Worth stating plainly that `[]` is a GUESS — the oracle
can't confirm it, because asking the question kills the oracle. If upstream fixes this with
different semantics (coerce the column, or skip the row), fab follows upstream.
