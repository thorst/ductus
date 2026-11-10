//! Interfaces spread over threads (0.7.4.3.1): what makes `validate` and `test` take seconds at a
//! thousand interfaces, not a minute. Each thread has a single-threaded Tokio runtime of its own,
//! as the engine's workers do: an isolate must stay on the thread that made it, and deno_core
//! spawns tasks that must too. Results come back in the order of the work, as each is ready, so
//! what's printed is what one thread printed before, line for line.

use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

/// How many threads: one per CPU the system gives this process.
pub fn threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get())
}

/// Run `job` on each of `work`, spread over up to `threads` threads (each takes the next item
/// when it's done with one, so a slow item doesn't hold others back). `job` gets its thread's
/// runtime and a value the thread keeps between items (`Default` at first: an isolate it reuses,
/// say). `each` is called on this thread with every result, in `work`'s order, as soon as it and
/// all before it are done; its first error stops the work (items already started finish) and is
/// returned.
pub fn in_order<T, R, S>(
    work: &[T],
    threads: usize,
    job: impl Fn(&tokio::runtime::Runtime, &mut S, &T) -> R + Sync,
    mut each: impl FnMut(usize, R) -> Result<()>,
) -> Result<()>
where
    T: Sync,
    R: Send,
    S: Default,
{
    if work.is_empty() {
        return Ok(());
    }
    let next = AtomicUsize::new(0);
    let (send, receive) = mpsc::channel::<(usize, R)>();
    let mut waiting = BTreeMap::new();
    let mut done = 0;
    std::thread::scope(|scope| -> Result<()> {
        let mut handles = Vec::new();
        for _ in 0..threads.clamp(1, work.len()) {
            let (send, next, job) = (send.clone(), &next, &job);
            handles.push(scope.spawn(move || -> Result<()> {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                let mut state = S::default();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = work.get(i) else { return Ok(()) };
                    let result = job(&runtime, &mut state, item);
                    if send.send((i, result)).is_err() {
                        return Ok(()); // the caller stopped listening
                    }
                }
            }));
        }
        drop(send);
        // Every thread's sender is gone once all are done (or one panicked).
        let mut failed = None;
        'results: for (i, result) in receive.iter() {
            waiting.insert(i, result);
            while let Some(result) = waiting.remove(&done) {
                if let Err(e) = each(done, result) {
                    failed = Some(e);
                    next.store(work.len(), Ordering::Relaxed); // nothing more is started
                    break 'results;
                }
                done += 1;
            }
        }
        drop(receive); // threads still sending stop
        if let Some(e) = failed {
            for handle in handles {
                let _ = handle.join();
            }
            return Err(e);
        }
        for handle in handles {
            match handle.join() {
                Ok(started) => started?,
                Err(_) => bail!("a thread checking interfaces panicked (its output is above, on stderr)"),
            }
        }
        Ok(())
    })?;
    if done < work.len() {
        bail!("only {done} of {} interfaces were checked", work.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_come_in_the_order_of_the_work() {
        // The first items take longest: done last, still given first.
        let work: Vec<u64> = (0..40).collect();
        let mut seen = Vec::new();
        in_order(
            &work,
            threads(),
            |_, count: &mut usize, &n| {
                std::thread::sleep(std::time::Duration::from_millis(40u64.saturating_sub(n)));
                *count += 1;
                n * 2
            },
            |i, r| {
                seen.push((i, r));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(seen, work.iter().map(|&n| (n as usize, n * 2)).collect::<Vec<_>>());
    }

    #[test]
    fn each_thread_keeps_its_state_and_has_a_runtime() {
        let work: Vec<usize> = (0..100).collect();
        let mut kept = 0;
        in_order(
            &work,
            threads(),
            |runtime, before: &mut Vec<usize>, &n| {
                before.push(n);
                runtime.block_on(async { tokio::task::yield_now().await });
                before.len()
            },
            |_, len| {
                kept = kept.max(len);
                Ok(())
            },
        )
        .unwrap();
        // With one thread it's 100; with more, at least one thread did several.
        assert!(kept >= 100usize.div_ceil(threads()), "kept {kept}");
    }

    #[test]
    fn a_panicking_job_is_an_error_not_a_hang() {
        let work = [1, 2, 3];
        let result =
            in_order(&work, threads(), |_, _: &mut (), &n| if n == 2 { panic!("two") } else { n }, |_, _| Ok(()));
        assert!(result.is_err());
    }

    #[test]
    fn the_first_error_stops_the_work() {
        let work: Vec<usize> = (0..1000).collect();
        let started = AtomicUsize::new(0);
        let result = in_order(
            &work,
            threads(),
            |_, _: &mut (), &n| {
                started.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(std::time::Duration::from_millis(1));
                n
            },
            |i, _| if i == 3 { bail!("stop at 3") } else { Ok(()) },
        );
        assert_eq!(result.unwrap_err().to_string(), "stop at 3");
        assert!(started.load(Ordering::Relaxed) < 1000, "the rest wasn't started");
    }
}
