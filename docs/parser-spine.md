# AA.4: the iterative expression spine (design, 2026-07-22)

issue4172 (a 144-deep nested-vector literal; upstream parses it and fails GRACEFULLY in eval)
meets our `MAX_DEPTH = 64` parser guard. chotchki's call: kill the cliff class (the M-phase
heap-bounded doctrine, parser edition), don't move it with a bigger budget.

## What recurses today (the AA.4.1 map)

The expr cascade in `parser/expr.rs`: `expr → ternary → binary → unary → exponent → call →
primary`, with genuine nesting (`depth + 1`) at:

- `[…]` vector elements and `(…)` parens — **the unbounded-in-practice dimension** (issue4172);
- `binary`'s rhs descent (`binary(i, bp + 1, depth + 1)`) — precedence climbing recurses per
  TIGHTER-binDING operator, so depth tracks operator-precedence height (~12) per nesting level,
  not chain length (same-level chains already loop);
- `unary` chains (`----x` recurses per sign);
- `ternary` arms, function-literal bodies, index exprs, call args.

Already ITERATIVE (leave alone): `let/assert/echo` chains (`chain_expr`'s loop), left-assoc
binary chains at one level, `call`'s postfix loop (`a.b[0](x)…`), statement-level single-child
module chains. STATEMENT nesting (module children / if arms) keeps `MAX_DEPTH` — real programs
don't nest statements 64 deep, and that guard also bounds the loader/hoist walkers.

## The machine (AA.4.2)

Replace the expr cascade's call recursion with an explicit-stack parser:

- **Operand stack** (`Vec<Expr>`) + **frame stack** (`Vec<Frame>`), where `Frame` is the
  continuation: `BinaryRhs{op, bp}`, `UnaryApply{op}`, `TernaryThen{cond}` / `TernaryEls{cond,
  then}`, `VectorElems{items}`, `RangeSecond/Third{…}`, `Paren`, `CallArgs{callee, args}`,
  `Index{node}`, `FnLitBody{params}`, comprehension frames.
- The driver loops: lex-dispatch the next atom into the operand stack, then unwind frames while
  binding powers allow — precedence climbing with the climb made explicit.
- `winnow` stays the token substrate; only the expr entry (`expr()`) changes shape. The statement
  parser keeps calling `expr(i, depth)` — the `depth` param stays in the signature for the
  statement-level guard, but expression nesting no longer consumes it.
- Depth cap for exprs: NONE structural; a sanity ceiling (e.g. 100k frames) guards adversarial
  input the way the eval step budget does — orders of magnitude past any real program, never a
  cliff a corpus file can hit.

## Downstream consumers of a now-deep AST (AA.4.3)

| consumer | today | verdict |
|---|---|---|
| `Expr::Drop` | already ITERATIVE (explicit work-stack, ast.rs) | nothing to do |
| eval (value + geometry) | heap-bounded (M phase, explicit-stack driver) | verify with issue4172's real shape: clean error, not abort |
| `fingerprint::hash_expr` | RECURSIVE — runs at ctx build on every fn def | must go iterative (same work-stack shape as Drop); a deep fn BODY is legal source |
| `print::write_expr` | deliberately recursive ("only prints asts we built") | now parseable-deep asts reach it via the customizer — make iterative OR budget-guard with a typed error; decide in-code, document either way |
| JIT lowering | numeric-subset walker declines big/weird bodies already | confirm it declines-not-crashes on deep input |

## Gates (AA.4.4)

issue4172 parses AND evals to a graceful error (their guard errors; ours must too); a corpus
seed ≥10× the old 64 cliff; the parser fuzz target generates deep nesting; the roundtrip
property (`parse(print(ast))`) holds at depth; full census re-sweep.
