# Nuances: shared pool

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

How the shared pool works: [The shared pool](../shared-pool.md).

## Interfaces share a global scope (level 2)
- **What:** in the shared pool many interfaces run in one JavaScript engine. Writing to `globalThis` or changing built-in prototypes affects them all. `ductus validate` warns (DX1, DX2).

## A shared library file is one copy per thread, not one per project (level 4)
- **What:** `lib/x.ts` imported by several interfaces is loaded once per isolate. Interfaces on the same pool thread share its top-level state (DX3); interfaces on different threads each see their own copy. A dedicated interface with `workers: 4` has four copies of its own module state too.
- **What to do:** never use module-level variables for data that must be consistent across the engine (counters, "last seen" values). Use the database.

## Module-level caches live until the engine restarts (level 2)
- **What:** a module-level `Map` keeps its contents across messages, and loses them on restart. It also counts against the isolate's heap limit.
- **What to do:** keep caches bounded, and make sure the code works with an empty cache.
