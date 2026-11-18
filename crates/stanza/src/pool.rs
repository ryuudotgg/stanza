#[cfg(test)]
use std::cell::Cell;
use std::io;
use std::panic;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use crate::files::reason;
use crate::format::catch_panic;

pub const THRESHOLD: usize = 64;
pub const DEFAULT_WORKERS: usize = 8;
pub const MOST_WORKERS: usize = 64;

// oxc's parser recurses on nested input, so the run asks for room for files nested up to the nesting cap, then less, with a lower cap, when the address space is limited.
pub const STACK_LADDER: [usize; 3] = [1 << 30, 1 << 28, 1 << 26];

const WORKER_NAME: &str = "stanza worker";

#[cfg(test)]
thread_local! {
    pub static SPAWNED: Cell<usize> = const { Cell::new(0) };
}

pub fn worker_count(file_count: usize, workers: Option<&str>, cores: usize) -> usize {
    if let Some(value) = workers
        && !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && let Ok(count) = value.parse::<usize>()
        && count <= MOST_WORKERS
    {
        return count;
    }

    if file_count < THRESHOLD {
        return 0;
    }

    DEFAULT_WORKERS.min(cores.saturating_sub(1))
}

pub fn cores() -> usize {
    thread::available_parallelism().map_or(1, usize::from)
}

pub fn on_largest_stack<T: Send>(work: impl Fn(Option<usize>) -> T + Sync) -> T {
    let work = &work;
    thread::scope(|scope| {
        for bytes in STACK_LADDER {
            let spawned = thread::Builder::new()
                .stack_size(bytes)
                .spawn_scoped(scope, move || work(Some(bytes)));

            if let Ok(thread) = spawned {
                return thread
                    .join()
                    .unwrap_or_else(|payload| panic::resume_unwind(payload));
            }
        }

        work(thread_stack())
    })
}

#[cfg(target_os = "macos")]
pub fn thread_stack() -> Option<usize> {
    let size = unsafe { libc::pthread_get_stacksize_np(libc::pthread_self()) };
    (size > 0).then_some(size)
}

#[cfg(target_os = "linux")]
pub fn thread_stack() -> Option<usize> {
    let mut attributes = std::mem::MaybeUninit::<libc::pthread_attr_t>::uninit();
    let mut size = 0;
    unsafe {
        // musl reports only the main thread's mapped stack, so the main thread reads its limit.
        let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
        if libc::syscall(libc::SYS_gettid) == libc::getpid() as libc::c_long
            && libc::getrlimit(libc::RLIMIT_STACK, limit.as_mut_ptr()) == 0
            && limit.assume_init().rlim_cur != libc::RLIM_INFINITY
        {
            return usize::try_from(limit.assume_init().rlim_cur).ok();
        }

        if libc::pthread_getattr_np(libc::pthread_self(), attributes.as_mut_ptr()) != 0 {
            return None;
        }

        let read = libc::pthread_attr_getstacksize(attributes.as_ptr(), &mut size);
        libc::pthread_attr_destroy(attributes.as_mut_ptr());
        (read == 0 && size > 0).then_some(size)
    }
}

fn unstarted_warning(count: usize, started: usize, error: &io::Error) -> String {
    format!(
        "{} of {count} worker threads failed to start, so the run went on without them: {}",
        count - started,
        reason(error)
    )
}

pub struct Pooled<O> {
    pub outcomes: Vec<O>,
    pub unstarted: Option<String>,
}

pub fn run_pooled<I: Sync, O: Send>(
    inputs: &[I],
    count: usize,
    stack: usize,
    defer: impl Fn(&I) -> bool + Sync,
    format: impl Fn(&I) -> O + Sync,
) -> Pooled<O> {
    let work = |input: &I| (!defer(input)).then(|| format(input));
    let next = AtomicUsize::new(0);
    let drain = || {
        let mut done: Vec<(usize, Option<O>)> = Vec::new();
        loop {
            let index = next.fetch_add(1, Ordering::Relaxed);
            let Some(input) = inputs.get(index) else {
                return done;
            };

            done.push((index, work(input)));
        }
    };

    let mut failed = None;
    let (mut results, started) = thread::scope(|scope| {
        let mut workers = Vec::new();
        for _ in 0..count {
            let spawned = thread::Builder::new()
                .name(WORKER_NAME.to_owned())
                .stack_size(stack)
                .spawn_scoped(scope, || catch_panic(drain));

            match spawned {
                Ok(worker) => {
                    #[cfg(test)]
                    SPAWNED.set(SPAWNED.get() + 1);
                    workers.push(worker);
                }
                Err(error) => failed = Some(error),
            }
        }

        let started = workers.len();

        let mut results = drain();
        for worker in workers {
            match worker.join() {
                Ok(Ok(done)) => results.extend(done),
                Ok(Err(panic)) => panic::resume_unwind(Box::new(panic)),
                Err(payload) => panic::resume_unwind(payload),
            }
        }

        (results, started)
    });

    let unstarted = failed.map(|error| unstarted_warning(count, started, &error));

    results.sort_unstable_by_key(|(index, _)| *index);
    let outcomes = results
        .into_iter()
        .zip(inputs)
        .map(|((_, outcome), input)| outcome.unwrap_or_else(|| format(input)))
        .collect();

    Pooled {
        outcomes,
        unstarted,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_worker_that_fails_to_start_names_the_count_and_the_reason() {
        let error = io::Error::from_raw_os_error(libc::EAGAIN);
        assert_eq!(
            unstarted_warning(3, 2, &error),
            "1 of 3 worker threads failed to start, so the run went on without them: resource temporarily unavailable"
        );
    }

    #[test]
    fn the_count_is_bounded_by_default_and_the_override_forces_it() {
        for (cores, count) in [
            (1, 0),
            (2, 1),
            (4, 3),
            (6, 5),
            (9, 8),
            (18, 8),
            (64, DEFAULT_WORKERS),
        ] {
            for (files, expected) in [
                (THRESHOLD - 1, 0),
                (THRESHOLD, count),
                (THRESHOLD * 2, count),
            ] {
                assert_eq!(
                    worker_count(files, None, cores),
                    expected,
                    "{files} files on {cores} cores"
                );
            }
        }

        assert_eq!(worker_count(THRESHOLD, Some("2"), 64), 2);
        for count in 0..=MOST_WORKERS {
            for files in [0, 1, THRESHOLD * 2, THRESHOLD * 4] {
                assert_eq!(worker_count(files, Some(&count.to_string()), 64), count);
            }
        }

        for (padded, count) in [("00", 0), ("01", 1), ("007", 7)] {
            assert_eq!(worker_count(1, Some(padded), 64), count, "{padded:?}");
        }

        let zeros_then_seven = format!("{}7", "0".repeat(200));
        assert_eq!(worker_count(THRESHOLD * 2, Some(&zeros_then_seven), 64), 7);
        assert_eq!(worker_count(THRESHOLD * 2, Some("64"), 64), 64);
        assert_eq!(worker_count(THRESHOLD * 2, Some("65"), 64), DEFAULT_WORKERS);

        for ignored in [
            "",
            "65",
            "-1",
            "+1",
            "1.5",
            " 2",
            "2 ",
            "2x",
            "x",
            "0x2",
            "١",
            "99999999999999999999",
        ] {
            assert_eq!(
                worker_count(THRESHOLD * 2, Some(ignored), 8),
                worker_count(THRESHOLD * 2, None, 8),
                "{ignored:?}"
            );
            assert_eq!(worker_count(1, Some(ignored), 64), 0, "{ignored:?}");
            assert_eq!(
                worker_count(THRESHOLD * 2, Some(ignored), 64),
                8,
                "{ignored:?}"
            );
        }
    }

    #[test]
    fn the_run_gets_the_top_rung_and_the_calling_thread_reads_its_own_stack() {
        assert_eq!(on_largest_stack(|stack| stack), Some(STACK_LADDER[0]));
        assert!(thread_stack().is_some());
    }

    #[test]
    fn every_worker_claims_from_sixty_four_inputs() {
        for size in [THRESHOLD, THRESHOLD * 2, THRESHOLD * 8] {
            let inputs: Vec<usize> = (0..size).collect();
            let formatted: Vec<AtomicUsize> = inputs.iter().map(|_| AtomicUsize::new(0)).collect();
            let claimants = Mutex::new(HashSet::new());
            let holding = (Mutex::new(0), Condvar::new());
            let pooled = run_pooled(
                &inputs,
                8,
                STACK_LADDER[0],
                |_| false,
                |input| {
                    formatted[*input].fetch_add(1, Ordering::Relaxed);
                    if claimants.lock().unwrap().insert(thread::current().id()) {
                        let (count, arrived) = &holding;
                        *count.lock().unwrap() += 1;
                        arrived.notify_all();
                        drop(
                            arrived
                                .wait_timeout_while(
                                    count.lock().unwrap(),
                                    Duration::from_secs(10),
                                    |count| *count < 9,
                                )
                                .unwrap(),
                        );
                    }

                    *input
                },
            );

            assert_eq!(*holding.0.lock().unwrap(), 9, "{size} inputs");
            assert!(
                formatted
                    .iter()
                    .all(|count| count.load(Ordering::Relaxed) == 1)
            );
            assert_eq!(pooled.outcomes, inputs);
        }
    }

    #[test]
    fn a_panic_on_a_worker_reaches_the_caller_with_its_message() {
        let inputs: Vec<usize> = (0..THRESHOLD * 4).collect();
        let worker = (Mutex::new(None), Condvar::new());
        let caught = catch_panic(|| {
            run_pooled(
                &inputs,
                2,
                STACK_LADDER[0],
                |_| false,
                |input| {
                    let (claimed, claim) = &worker;
                    if thread::current().name() == Some(WORKER_NAME) {
                        claimed.lock().unwrap().get_or_insert(*input);
                        claim.notify_all();
                        panic!("worker\n  failed");
                    }

                    drop(
                        claim
                            .wait_timeout_while(
                                claimed.lock().unwrap(),
                                Duration::from_secs(10),
                                |claimed| claimed.is_none(),
                            )
                            .unwrap(),
                    );
                },
            )
        });

        assert!(worker.0.lock().unwrap().is_some());
        assert_eq!(caught.err().as_deref(), Some("worker failed"));
    }

    #[test]
    fn outcomes_keep_input_order_and_deferred_items_run_on_main_after_the_pool() {
        let inputs: Vec<usize> = (0..THRESHOLD * 5 + 3).collect();
        for count in [1, 3, 64] {
            let main = thread::current().id();
            let workers = std::sync::Mutex::new(std::collections::HashSet::new());
            let pooled = run_pooled(
                &inputs,
                count,
                STACK_LADDER[0],
                |input| input % 7 == 0,
                |input| {
                    let on_main = thread::current().id() == main;
                    workers.lock().unwrap().insert(thread::current().id());
                    thread::sleep(std::time::Duration::from_micros(100));
                    (*input, on_main || input % 7 != 0)
                },
            );

            let expected: Vec<(usize, bool)> = inputs.iter().map(|input| (*input, true)).collect();
            assert_eq!(pooled.outcomes, expected);
            assert!(pooled.unstarted.is_none());
            assert!(workers.lock().unwrap().iter().any(|id| *id != main));
        }
    }
}
