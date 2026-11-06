//! Runaway-script protection: a time budget for every call into JavaScript, and a heap limit
//! per isolate.
//!
//! Many interfaces share one isolate on one thread, so an endless loop or a memory blow-up in
//! one of them would stall (or crash) all of them. Each call into V8 is *armed* with a deadline.
//! One watchdog thread for the whole process checks the deadlines; when one passes, it asks the
//! isolate for an interrupt. The interrupt runs on the isolate's own thread, inside the runaway
//! code: it reads the JavaScript stack (to tell which interface it is) and terminates
//! execution. Terminating from the interrupt rather than from the watchdog means a call that
//! finished in the meantime is never hit by a termination meant for it.
//!
//! The event loop runs continuations of any interface, so it's armed with the shortest budget
//! of all; when that passes, the interrupt finds whose code is running and, if that interface
//! is allowed longer, moves the deadline instead of stopping it.
//!
//! Near the heap limit, V8 asks us for more room: we give a little (so the process doesn't
//! die) and stop the running code the same way.
//!
//! While a debugger is attached ([`DebugSwitch`]) the time limit is off: a call paused at a
//! breakpoint would otherwise be stopped the moment it went on. The heap limit stays. When the
//! last debugger detaches, the call in progress gets its whole budget again from that moment, so
//! an endless loop left running is stopped within the interface's timeout, as any runaway.

use deno_core::v8;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

/// How often the watchdog checks deadlines. Timeouts are seconds, so this is plenty.
const TICK: Duration = Duration::from_millis(10);
/// Frames read from the stack to find the runaway interface.
const FRAMES: usize = 64;

/// Why a call was stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// Ran longer than its time budget without pausing.
    Timeout(Duration),
    /// The isolate's heap reached its limit (in MB).
    Heap(usize),
}

/// A call that was stopped: why, and whose code it was (index of the interface, if found).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Trip {
    pub cause: Cause,
    pub interface: Option<usize>,
}

/// An interface's modules' URLs (its script, each destination's) and its folder (with a
/// trailing `/`), to recognize its code on the stack.
pub(crate) type Modules = (Vec<String>, String);

/// What the guard knows about each interface in the isolate (indexed like `InterfaceId`).
struct Interface {
    timeout: Duration,
    /// None for test suites.
    module: Option<Modules>,
}

/// State shared between an isolate, the watchdog thread and the interrupts.
struct Shared {
    handle: v8::IsolateHandle,
    /// Generation of the call in progress (0 = no call).
    armed: AtomicU64,
    /// When the call in progress started and must end (ms since `EPOCH`), and its budget.
    started: AtomicU64,
    deadline: AtomicU64,
    budget: AtomicU64,
    /// The call may be any interface's code: extend to the right budget once it's known.
    extendable: AtomicBool,
    /// Generation the watchdog or heap callback already asked to stop (so it asks once).
    requested: AtomicU64,
    heap_mb: usize,
    interfaces: Mutex<Vec<Interface>>,
    trip: Mutex<Option<Trip>>,
    /// Debuggers attached now: while any is, no call is stopped for time.
    debuggers: AtomicUsize,
}

/// Turns an isolate's time limit off while debuggers are attached (`attached`/`detached` from
/// any thread: the inspector server's).
#[derive(Clone)]
pub struct DebugSwitch(Arc<Shared>);

impl DebugSwitch {
    /// A debugger attached: from now on nothing is stopped for running too long.
    pub fn attached(&self) {
        self.0.debuggers.fetch_add(1, Ordering::AcqRel);
    }

    /// A debugger detached. When it was the last, the call in progress (if any) may run for its
    /// whole budget from now, then is stopped as any runaway.
    pub fn detached(&self) {
        let shared = &self.0;
        if shared.debuggers.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        let now = now_ms();
        shared.started.store(now, Ordering::Relaxed);
        shared.deadline.store(now + shared.budget.load(Ordering::Relaxed), Ordering::Relaxed);
        shared.requested.store(0, Ordering::Release);
    }

    /// Whether a debugger is attached now.
    pub fn is_attached(&self) -> bool {
        self.0.debuggers.load(Ordering::Acquire) > 0
    }
}

/// One isolate's guard. Owned by `Isolate`; all methods are called on the isolate's thread.
pub(crate) struct Guard {
    shared: Arc<Shared>,
    generation: u64,
}

struct Request {
    shared: Arc<Shared>,
    generation: u64,
    cause: Cause,
}

/// The context interface code runs in, kept in an isolate slot for reading the stack.
#[derive(Clone)]
struct MainContext(v8::Global<v8::Context>);

impl Guard {
    /// Guard `isolate` (created with a heap limit of `heap_mb`), whose code runs in `context`.
    pub fn new(isolate: &mut v8::OwnedIsolate, context: v8::Global<v8::Context>, heap_mb: usize) -> Self {
        isolate.set_slot(MainContext(context));
        let shared = Arc::new(Shared {
            handle: isolate.thread_safe_handle(),
            armed: AtomicU64::new(0),
            started: AtomicU64::new(0),
            deadline: AtomicU64::new(0),
            budget: AtomicU64::new(0),
            extendable: AtomicBool::new(false),
            requested: AtomicU64::new(0),
            heap_mb,
            interfaces: Mutex::new(Vec::new()),
            trip: Mutex::new(None),
            debuggers: AtomicUsize::new(0),
        });
        watchdog().lock().unwrap_or_else(|e| e.into_inner()).push(Arc::downgrade(&shared));
        Self { shared, generation: 0 }
    }

    /// What the inspector server turns the time limit off with.
    pub fn debug_switch(&self) -> DebugSwitch {
        DebugSwitch(self.shared.clone())
    }

    /// Register the next interface (`module`: its modules' URLs and its folder) and its time budget.
    pub fn add(&self, module: Option<Modules>, timeout: Duration) {
        self.shared.interfaces().push(Interface { timeout, module });
    }

    /// For the near-heap-limit callback: stop the running call, and return the new limit.
    /// V8 crashes the process if we don't raise it, so we give a quarter more each time; the
    /// stopped interface's garbage is then collected (or, if it holds on to it, at least it
    /// can't grow any more because it no longer runs).
    pub fn heap_callback(&self) -> impl FnMut(usize, usize) -> usize + 'static {
        let shared = self.shared.clone();
        move |current, initial| {
            let generation = shared.armed.load(Ordering::Acquire);
            if generation != 0 {
                shared.request(generation, Cause::Heap(shared.heap_mb));
            }
            current + (initial / 4).max(16 << 20)
        }
    }

    /// A call of `interface`'s code is starting.
    pub fn arm_for(&mut self, interface: usize) {
        let budget = self.shared.interfaces()[interface].timeout;
        self.arm(budget, false);
    }

    /// Code of any interface may run (the event loop): start with the shortest budget.
    pub fn arm_any(&mut self, fallback: Duration) {
        let budget = self.shared.interfaces().iter().map(|i| i.timeout).min().unwrap_or(fallback);
        self.arm(budget, true);
    }

    /// A call into JavaScript is starting: it may run for `budget` (or, if `extendable`, for
    /// the budget of the interface whose code turns out to be running).
    pub fn arm(&mut self, budget: Duration, extendable: bool) {
        self.generation += 1;
        let shared = &self.shared;
        let now = now_ms();
        let budget = budget.as_millis() as u64;
        shared.extendable.store(extendable, Ordering::Relaxed);
        shared.budget.store(budget, Ordering::Relaxed);
        shared.started.store(now, Ordering::Relaxed);
        shared.deadline.store(now + budget, Ordering::Relaxed);
        shared.armed.store(self.generation, Ordering::Release);
    }

    /// The call is over. If it was stopped, returns why (and the isolate is usable again).
    pub fn disarm(&mut self, isolate: &mut v8::OwnedIsolate) -> Option<Trip> {
        self.shared.armed.store(0, Ordering::Release);
        let trip = self.shared.trip.lock().unwrap_or_else(|e| e.into_inner()).take()?;
        // Interrupts run on this thread, so none can arrive now for the call that just ended.
        isolate.cancel_terminate_execution();
        if matches!(trip.cause, Cause::Heap(_)) {
            isolate.low_memory_notification(); // collect what the stopped call left behind
        }
        Some(trip)
    }
}

impl Shared {
    fn interfaces(&self) -> std::sync::MutexGuard<'_, Vec<Interface>> {
        self.interfaces.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Ask the isolate to stop call `generation` (once), from any thread.
    fn request(self: &Arc<Self>, generation: u64, cause: Cause) {
        if self.requested.swap(generation, Ordering::AcqRel) == generation {
            return;
        }
        let request = Box::into_raw(Box::new(Request { shared: self.clone(), generation, cause }));
        if !self.handle.request_interrupt(interrupt, request.cast()) {
            // The isolate is gone; the interrupt will never run.
            drop(unsafe { Box::from_raw(request) });
        }
    }

    /// The innermost interface on the stack: its module files first, then anything in its folder.
    fn whose(&self, scripts: &[String]) -> Option<usize> {
        let interfaces = self.interfaces();
        let find = |matches: &dyn Fn(&str, &Modules) -> bool| {
            scripts.iter().find_map(|script| {
                interfaces.iter().position(|i| i.module.as_ref().is_some_and(|m| matches(script, m)))
            })
        };
        find(&|script, (urls, _)| urls.iter().any(|u| u == script))
            .or_else(|| find(&|script, (_, folder)| script.starts_with(folder)))
    }
}

/// Runs on the isolate's thread, inside the JavaScript that's running.
unsafe extern "C" fn interrupt(mut isolate: v8::UnsafeRawIsolatePtr, data: *mut c_void) {
    // SAFETY: `data` is the `Request` boxed in `Shared::request`, handed to us exactly once.
    let request = unsafe { Box::from_raw(data.cast::<Request>()) };
    let shared = &request.shared;
    if shared.armed.load(Ordering::Acquire) != request.generation {
        return; // that call already finished: this is someone else's code now
    }
    if let Cause::Timeout(_) = request.cause
        && shared.debuggers.load(Ordering::Acquire) > 0
    {
        // A debugger attached after it was asked: no time limit now (detaching re-arms it).
        shared.requested.store(0, Ordering::Release);
        return;
    }
    // SAFETY: V8 calls interrupts on the isolate's thread with the isolate entered.
    let isolate = unsafe { v8::Isolate::ref_from_raw_isolate_ptr_mut(&mut isolate) };
    let interface = shared.whose(&stack(isolate));
    let mut cause = request.cause;
    if let Cause::Timeout(_) = cause
        && shared.extendable.load(Ordering::Relaxed)
    {
        // Allowed as long as its own interface allows (the longest of all, if unknown).
        let interfaces = shared.interfaces();
        let own = match interface {
            Some(i) => interfaces[i].timeout,
            None => interfaces.iter().map(|i| i.timeout).max().unwrap_or_default(),
        };
        drop(interfaces);
        let end = shared.started.load(Ordering::Relaxed) + own.as_millis() as u64;
        if now_ms() < end {
            shared.budget.store(own.as_millis() as u64, Ordering::Relaxed);
            shared.deadline.store(end, Ordering::Relaxed);
            shared.requested.store(0, Ordering::Release); // the watchdog may ask again
            return;
        }
        cause = Cause::Timeout(own);
    }
    *shared.trip.lock().unwrap_or_else(|e| e.into_inner()) = Some(Trip { cause, interface });
    isolate.terminate_execution();
}

/// Script URLs on the current JavaScript stack, innermost first.
fn stack(isolate: &mut v8::Isolate) -> Vec<String> {
    let Some(MainContext(context)) = isolate.get_slot::<MainContext>().cloned() else { return Vec::new() };
    v8::scope_with_context!(scope, isolate, &context);
    let mut scripts = Vec::new();
    if let Some(trace) = v8::StackTrace::current_stack_trace(scope, FRAMES) {
        for i in 0..trace.get_frame_count() {
            if let Some(name) = trace.get_frame(scope, i).and_then(|f| f.get_script_name(scope)) {
                scripts.push(name.to_rust_string_lossy(scope));
            }
        }
    }
    scripts
}

/// Every guarded isolate in the process; the watchdog thread starts with the first one.
fn watchdog() -> &'static Mutex<Vec<Weak<Shared>>> {
    static GUARDS: OnceLock<&'static Mutex<Vec<Weak<Shared>>>> = OnceLock::new();
    GUARDS.get_or_init(|| {
        let guards: &'static Mutex<Vec<Weak<Shared>>> = Box::leak(Box::new(Mutex::new(Vec::new())));
        std::thread::Builder::new()
            .name("script-watchdog".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(TICK);
                    let now = now_ms();
                    let mut list = guards.lock().unwrap_or_else(|e| e.into_inner());
                    list.retain(|guard| {
                        let Some(shared) = guard.upgrade() else { return false };
                        let generation = shared.armed.load(Ordering::Acquire);
                        let debugged = shared.debuggers.load(Ordering::Acquire) > 0;
                        if generation != 0 && !debugged && now > shared.deadline.load(Ordering::Relaxed) {
                            let budget = Duration::from_millis(shared.budget.load(Ordering::Relaxed));
                            // The deadline read may belong to a newer call; the interrupt re-checks.
                            if shared.armed.load(Ordering::Acquire) == generation {
                                shared.request(generation, Cause::Timeout(budget));
                            }
                        }
                        true
                    });
                }
            })
            .expect("starting the script watchdog thread");
        guards
    })
}

fn now_ms() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64
}
