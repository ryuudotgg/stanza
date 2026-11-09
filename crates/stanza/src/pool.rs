#[cfg(test)]
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

pub const THRESHOLD: usize = 256;
pub const DEFAULT_WORKERS: usize = 3;
pub const CHUNK_SIZE: usize = 64;
const MOST_WORKERS: usize = 64;

// oxc's parser recurses on nested input, so every thread that formats gets room for the deep files the reference parses.
pub const STACK_BYTES: usize = 1 << 30;

const WORKER_NAME: &str = "stanza worker";

#[cfg(test)]
thread_local! {
    pub static SPAWNED: Cell<usize> = const { Cell::new(0) };
}

pub fn worker_count(
    file_count: usize,
    workers: Option<&str>,
    maximum: usize,
    cores: usize,
) -> usize {
    if let Some(value) = workers
        && !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
    {
        let digits = value.trim_start_matches('0');
        let count = if digits.len() > 2 {
            usize::MAX
        } else {
            digits.parse().unwrap_or(0)
        };

        if count <= MOST_WORKERS {
            return count;
        }
    }

    if file_count < THRESHOLD {
        return 0;
    }

    maximum.min(cores.saturating_sub(1))
}

pub fn cores() -> usize {
    thread::available_parallelism().map_or(1, usize::from)
}

pub struct Pooled<O> {
    pub outcomes: Vec<O>,
    pub errors: Vec<String>,
}

pub fn run_pooled<I: Sync, O: Send>(
    inputs: &[I],
    count: usize,
    defer: impl Fn(&I) -> bool + Sync,
    format: impl Fn(&I) -> O + Sync,
) -> Pooled<O> {
    let work = |input: &I| (!defer(input)).then(|| format(input));
    let chunks: Vec<&[I]> = inputs.chunks(CHUNK_SIZE).collect();
    let next = AtomicUsize::new(0);
    let drain = || {
        let mut done: Vec<(usize, Vec<Option<O>>)> = Vec::new();
        loop {
            let index = next.fetch_add(1, Ordering::Relaxed);
            let Some(chunk) = chunks.get(index) else {
                return done;
            };

            done.push((index, chunk.iter().map(&work).collect()));
        }
    };

    let mut errors = Vec::new();
    let mut results = thread::scope(|scope| {
        let mut workers = Vec::new();
        for _ in 0..count {
            let spawned = thread::Builder::new()
                .name(WORKER_NAME.to_owned())
                .stack_size(STACK_BYTES)
                .spawn_scoped(scope, drain);

            match spawned {
                Ok(worker) => {
                    #[cfg(test)]
                    SPAWNED.set(SPAWNED.get() + 1);
                    workers.push(worker);
                }
                Err(error) => errors.push(format!(
                    "a worker thread failed to start, so main did its share: {error}"
                )),
            }
        }

        let mut results = drain();
        for worker in workers {
            results.extend(
                worker
                    .join()
                    .expect("format_file catches a panic on its own file"),
            );
        }

        results
    });

    results.sort_by_key(|(index, _)| *index);
    let outcomes = results
        .into_iter()
        .flat_map(|(_, settled)| settled)
        .zip(inputs)
        .map(|(outcome, input)| outcome.unwrap_or_else(|| format(input)))
        .collect();

    Pooled { outcomes, errors }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_count_is_bounded_by_default_and_the_override_forces_it() {
        assert_eq!(worker_count(THRESHOLD - 1, None, 8, 64), 0);
        assert_eq!(
            worker_count(THRESHOLD, None, DEFAULT_WORKERS, 64),
            DEFAULT_WORKERS
        );
        for (maximum, cores, count) in [(3, 1, 0), (3, 2, 1), (3, 4, 3), (8, 4, 3), (0, 64, 0)] {
            assert_eq!(
                worker_count(THRESHOLD, None, maximum, cores),
                count,
                "{maximum} of {cores}"
            );
        }

        assert_eq!(worker_count(THRESHOLD, Some("2"), 8, 64), 2);
        for count in [0, 1, 7, 64] {
            for files in [0, 1, THRESHOLD * 4] {
                assert_eq!(worker_count(files, Some(&count.to_string()), 3, 64), count);
            }
        }

        assert_eq!(worker_count(1, Some("007"), 3, 64), 7);
        for ignored in [
            "",
            "65",
            "-1",
            "1.5",
            " 2",
            "2 ",
            "x",
            "0x2",
            "١",
            "99999999999999999999",
        ] {
            assert_eq!(
                worker_count(THRESHOLD * 2, Some(ignored), 3, 8),
                worker_count(THRESHOLD * 2, None, 3, 8),
                "{ignored:?}"
            );
        }
    }

    #[test]
    fn outcomes_keep_input_order_and_deferred_items_run_on_main_after_the_pool() {
        let inputs: Vec<usize> = (0..CHUNK_SIZE * 5 + 3).collect();
        for count in [1, 3, 64] {
            let main = thread::current().id();
            let workers = std::sync::Mutex::new(std::collections::HashSet::new());
            let pooled = run_pooled(
                &inputs,
                count,
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
            assert!(pooled.errors.is_empty());
            assert!(workers.lock().unwrap().iter().any(|id| *id != main));
        }
    }
}
