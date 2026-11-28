//! What an isolate's heap holds and what its garbage collections cost, read by V8's own GC
//! callbacks: on the isolate's thread, at each collection, never on a timer or per message.
//!
//! Other threads (`/metrics`, `GET /engine`) read the numbers from atomics without waiting for the
//! isolate's thread, which may be busy in a transform. They are as of the last collection: an
//! isolate that allocates nothing collects nothing, and its numbers stand still (nothing changed).
//!
//! The leak signal is `old_live`: the old generation's size right after a full (mark-compact)
//! collection, which is what the code still holds on to. Between full collections the old
//! generation also holds garbage not yet collected, which is why reading the heap at an arbitrary
//! moment says little (the reason `worker::LOAD_COST` counts code instead of reading the heap).

use deno_core::v8;
use std::cell::Cell;
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// The collections counted: scavenges and minor mark-sweeps are minor, mark-compacts major.
/// Incremental marking's steps (`kGCTypeIncrementalMarking`) run inside the code between the
/// pauses and aren't counted as pauses; the major one is the final, atomic pause.
fn counted() -> v8::GCType {
    v8::GCType::kGCTypeScavenge | v8::GCType::kGCTypeMinorMarkSweep | v8::GCType::kGCTypeMarkSweepCompact
}

/// A collection's kind, as `/metrics` labels it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GcKind {
    /// The young generation (scavenge, minor mark-sweep): frequent, short.
    Minor,
    /// Everything (mark-compact): rarer, longer, and what measures what's live.
    Major,
}

impl GcKind {
    pub const ALL: [GcKind; 2] = [GcKind::Minor, GcKind::Major];

    pub fn label(self) -> &'static str {
        match self {
            GcKind::Minor => "minor",
            GcKind::Major => "major",
        }
    }
}

/// One isolate's heap and collections, shared with the threads that report them.
#[derive(Debug, Default)]
pub struct HeapWatch {
    used: AtomicU64,
    total: AtomicU64,
    limit: AtomicU64,
    allocated: AtomicU64,
    old_live: AtomicU64,
    collections: [AtomicU64; 2],
    pause_nanos: [AtomicU64; 2],
}

/// A reading of a `HeapWatch`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Heap {
    /// Bytes the heap's objects take, after the last collection (live and not yet collected).
    pub used: u64,
    /// Bytes V8 has reserved for the heap (`used` and room to grow).
    pub total: u64,
    /// The heap's limit (`isolates.heap_mb`, raised by a quarter each time the guard stops code
    /// that reached it).
    pub limit: u64,
    /// Bytes allocated since the isolate was made (a counter: its rate is the allocation rate).
    pub allocated: u64,
    /// The old generation right after the last full collection: what the code keeps. 0 before the
    /// first one.
    pub old_live: u64,
    /// Collections since the isolate was made, by kind (`GcKind as usize`).
    pub collections: [u64; 2],
    /// Time the code was paused by them, by kind, in nanoseconds.
    pub pause_nanos: [u64; 2],
}

impl HeapWatch {
    pub fn read(&self) -> Heap {
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        Heap {
            used: get(&self.used),
            total: get(&self.total),
            limit: get(&self.limit),
            allocated: get(&self.allocated),
            old_live: get(&self.old_live),
            collections: [get(&self.collections[0]), get(&self.collections[1])],
            pause_nanos: [get(&self.pause_nanos[0]), get(&self.pause_nanos[1])],
        }
    }

    /// Record the heap's statistics now (on the isolate's thread).
    fn record(&self, isolate: &mut v8::Isolate) {
        let s = isolate.get_heap_statistics();
        let set = |a: &AtomicU64, v: usize| a.store(v as u64, Ordering::Relaxed);
        set(&self.used, s.used_heap_size());
        set(&self.total, s.total_heap_size());
        set(&self.limit, s.heap_size_limit());
        self.allocated.store(s.total_allocated_bytes(), Ordering::Relaxed);
    }

    fn record_old_live(&self, isolate: &mut v8::Isolate) {
        let old = (0..isolate.number_of_heap_spaces())
            .filter_map(|i| isolate.get_heap_space_statistics(i))
            .find(|s| s.space_name().to_bytes() == b"old_space")
            .map_or(0, |s| s.space_used_size());
        self.old_live.store(old as u64, Ordering::Relaxed);
    }
}

/// What V8's callbacks are handed: the watch, and when the collection under way began. Owned by
/// the `Isolate` and dropped after its `JsRuntime` (field order), so V8 never calls into a freed one.
pub(crate) struct GcCallbacks {
    watch: Arc<HeapWatch>,
    began: Cell<Option<Instant>>,
}

impl GcCallbacks {
    /// Watch `isolate`'s collections from now on.
    pub(crate) fn install(isolate: &mut v8::Isolate) -> Box<GcCallbacks> {
        let callbacks = Box::new(GcCallbacks { watch: Arc::default(), began: Cell::new(None) });
        let data = std::ptr::from_ref::<GcCallbacks>(&callbacks).cast_mut().cast::<c_void>();
        isolate.add_gc_prologue_callback(prologue, data, counted());
        isolate.add_gc_epilogue_callback(epilogue, data, counted());
        // The numbers before any collection: the isolate as made.
        callbacks.watch.record(isolate);
        callbacks
    }

    pub(crate) fn watch(&self) -> Arc<HeapWatch> {
        self.watch.clone()
    }
}

fn kind(gc: v8::GCType) -> GcKind {
    if gc == v8::GCType::kGCTypeMarkSweepCompact { GcKind::Major } else { GcKind::Minor }
}

unsafe extern "C" fn prologue(_: v8::UnsafeRawIsolatePtr, _: v8::GCType, _: v8::GCCallbackFlags, data: *mut c_void) {
    // SAFETY: `data` is the `GcCallbacks` `install` registered, alive while the isolate is; V8
    // calls GC callbacks on the isolate's thread, the only one touching `began`.
    let callbacks = unsafe { &*data.cast::<GcCallbacks>() };
    callbacks.began.set(Some(Instant::now()));
}

unsafe extern "C" fn epilogue(
    mut isolate: v8::UnsafeRawIsolatePtr,
    gc: v8::GCType,
    _: v8::GCCallbackFlags,
    data: *mut c_void,
) {
    // SAFETY: as in `prologue`.
    let callbacks = unsafe { &*data.cast::<GcCallbacks>() };
    let watch = &callbacks.watch;
    let kind = kind(gc) as usize;
    if let Some(began) = callbacks.began.take() {
        watch.pause_nanos[kind].fetch_add(began.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    watch.collections[kind].fetch_add(1, Ordering::Relaxed);
    // SAFETY: V8 calls GC callbacks on the isolate's thread with the isolate entered; reading its
    // statistics there is allowed (Node's `v8.GCProfiler` does the same), allocating isn't.
    let isolate = unsafe { v8::Isolate::ref_from_raw_isolate_ptr_mut(&mut isolate) };
    watch.record(isolate);
    if kind == GcKind::Major as usize {
        watch.record_old_live(isolate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the epilogue adds to a collection: its reads timed alone, against what a collection
    /// itself takes. `cargo test --release -p ductus-script heap::tests -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement"]
    fn what_the_callbacks_cost() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let _guard = rt.enter();
        let mut host = crate::Isolate::host();
        let watch = host.heap_watch();
        let isolate = host.runtime.v8_isolate();
        const N: u32 = 100_000;
        let started = Instant::now();
        for _ in 0..N {
            watch.record(isolate);
        }
        let minor = started.elapsed() / N;
        let started = Instant::now();
        for _ in 0..N {
            watch.record(isolate);
            watch.record_old_live(isolate);
        }
        let major = started.elapsed() / N;
        let source = "let n = 0; for (let i = 0; i < 2000000; i++) n += [i, String(i), { i }].length; n";
        host.runtime.execute_script("churn", source.to_string()).unwrap();
        let h = watch.read();
        let mean = |k: GcKind| h.pause_nanos[k as usize] / h.collections[k as usize].max(1);
        println!(
            "epilogue's reads: minor {minor:?}, major {major:?}; collections: {} minor (mean pause {} µs), {} major (mean pause {} µs), {} MB allocated",
            h.collections[0],
            mean(GcKind::Minor) / 1000,
            h.collections[1],
            mean(GcKind::Major) / 1000,
            h.allocated >> 20,
        );
    }
}
