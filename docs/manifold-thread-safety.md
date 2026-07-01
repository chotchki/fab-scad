# Manifold + threads: why `kernel::Solid` is `!Send`

Short version: the `manifold3d` binding declares `unsafe impl Send + Sync for Manifold`, but the
claim isn't airtight — there's an unlocked lazy mutation on a SHARED node. Rather than depend on it,
`kernel::Solid` is `!Send`/`!Sync` by construction. Thread boundaries carry inert mesh data, never a
live `Solid`.

## The hazard (verified against the vendored C++)

- **`clone` shares the CSG node, it doesn't deep-copy.** `manifold_copy` → the `Manifold` copy ctor,
  which does `pNode_ = other.pNode_` — a `shared_ptr` copy (manifold.cpp). So an original and its
  clone point at the same `CsgNode`.
- **The per-`Manifold` mutex doesn't cover the shared leaf.** Each `Manifold` has a `pNodeMutex_`
  guarding its own `pNode_`/`ctx_` access (LoadPNode/StorePNode) — good for concurrent access to ONE
  object. But it doesn't guard what's INSIDE a shared node.
- **`CsgLeafNode::GetImpl() const` mutates without a lock.** When the leaf has a non-identity
  `transform_`, `GetImpl()` bakes it lazily: `pImpl_ = make_shared<Impl>(pImpl_->Transform(...))` —
  writing a `mutable` member, no lock (csg_tree.cpp). Two threads that share a transform-pending leaf
  (via clone) and both force evaluation race on `pImpl_`. That's a data race → UB.

Why it's a LATENT trap: a freshly-imported leaf has an identity transform, so `GetImpl()` takes the
no-mutation early return — accidentally safe. The race only wakes up once the cached solid has been
`.translate()`/`.rotate()`/`.transform()`d (seat-on-bed, spread, connector placement — all of which
the slicer does) AND that solid is cloned across threads. It would pass every single-threaded test
and then bite in the reactive GUI.

`Send`-by-move (transfer exclusive ownership, keep no clone elsewhere) is actually sound. `Sync`
(shared `&Manifold`) and clone-to-two-threads are the unsound patterns.

## The decision

`kernel::Solid` carries a `PhantomData<*const ()>` → it is `!Send` + `!Sync`. The compiler REFUSES to
move one across a thread boundary; a `compile_fail` doctest locks that in. The rule that falls out:

> To do geometry work off the main thread, hand the worker INERT MESH DATA — STL bytes, or vertex +
> index buffers — and rebuild the `Solid` on the far side. Never move or share a `Solid`.

No `CsgNode` is ever shared across threads, so neither the `Send` nor the `Sync` impl is load-bearing
and the lazy-eval race cannot happen — regardless of what upstream does with its locking.

## Implication for the reactive GUI (Track C 11.10)

The reslice worker gets the base as mesh data (the cached STL bytes / welded buffers), re-imports it
(~12 ms), slices + applies connectors, and sends back the piece meshes as plain `Vec`s. The main
thread never touches a `Solid` the worker touches. Costs one extra weld per reslice — nothing against
the 0.35 s debounce, and the UI thread stays smooth.
