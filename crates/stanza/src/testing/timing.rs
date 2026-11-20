// A 4x larger input costs 4x when linear, about 5x for an n log n pass, and 16x when quadratic.
pub(crate) const LINEAR_GROWTH_CAP: f64 = 8.0;

pub(crate) fn thread_cpu_ms() -> f64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };

    let status = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) };
    assert_eq!(status, 0, "the thread clock reads");
    time.tv_sec as f64 * 1_000.0 + time.tv_nsec as f64 / 1_000_000.0
}

pub(crate) fn cpu_ms(work: impl FnOnce()) -> f64 {
    let started = thread_cpu_ms();
    work();
    thread_cpu_ms() - started
}

pub(crate) fn ratio<K>(cap: f64) -> impl Fn(&[(K, f64)]) -> bool {
    move |samples| samples[samples.len() - 1].1 <= samples[0].1 * cap
}

pub(crate) fn slope(limit: f64) -> impl Fn(&[(usize, f64)]) -> bool {
    move |samples| growth(samples) <= limit
}

fn growth(samples: &[(usize, f64)]) -> f64 {
    let points: Vec<(f64, f64)> = samples
        .iter()
        .map(|&(size, ms)| ((size as f64).ln(), ms.ln()))
        .collect();

    let count = points.len() as f64;
    let mean_x = points.iter().map(|point| point.0).sum::<f64>() / count;
    let mean_y = points.iter().map(|point| point.1).sum::<f64>() / count;
    let covariance: f64 = points
        .iter()
        .map(|(x, y)| (x - mean_x) * (y - mean_y))
        .sum();

    let variance: f64 = points.iter().map(|(x, _)| (x - mean_x).powi(2)).sum();
    covariance / variance
}

const ATTEMPTS: usize = 3;

type Attempt<K> = Vec<(K, f64)>;

fn attempts<K: Copy, T>(
    keys: &[K],
    rounds: usize,
    bound: impl Fn(&[(K, f64)]) -> bool,
    build: impl FnMut(K) -> T,
    mut sample: impl FnMut(&T) -> f64,
) -> Result<Attempt<K>, Vec<Attempt<K>>> {
    let inputs: Vec<T> = keys.iter().copied().map(build).collect();
    for input in &inputs {
        sample(input);
    }

    let mut failed = Vec::new();
    for _ in 0..ATTEMPTS {
        let mut best = vec![f64::INFINITY; inputs.len()];
        for _ in 0..rounds {
            for (index, input) in inputs.iter().enumerate() {
                best[index] = best[index].min(sample(input));
            }
        }

        let samples: Attempt<K> = keys.iter().copied().zip(best).collect();
        if bound(&samples) {
            return Ok(samples);
        }

        failed.push(samples);
    }

    Err(failed)
}

#[track_caller]
pub(crate) fn assert_bounded<K: Copy + std::fmt::Debug, T>(
    what: &str,
    keys: &[K],
    rounds: usize,
    bound: impl Fn(&[(K, f64)]) -> bool,
    build: impl FnMut(K) -> T,
    sample: impl FnMut(&T) -> f64,
) {
    if let Err(failed) = attempts(keys, rounds, bound, build, sample) {
        let named: Vec<String> = failed
            .iter()
            .enumerate()
            .map(|(index, samples)| format!("attempt {} {samples:?}", index + 1))
            .collect();
        panic!(
            "{what}: every attempt broke the bound, ms per key: {}",
            named.join(", ")
        );
    }
}

pub(crate) fn on_deep_stack<R: Send + 'static>(work: impl FnOnce() -> R + Send + 'static) -> R {
    std::thread::Builder::new()
        .stack_size(crate::pool::STACK_LADDER[0])
        .spawn(work)
        .unwrap()
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

fn linear_work(count: usize) -> usize {
    (0..count).fold(0, |sum, item| sum.wrapping_add(std::hint::black_box(item)))
}

fn quadratic_work(count: usize) -> usize {
    (0..count).fold(0, |sum, _| sum.wrapping_add(linear_work(count)))
}

fn shape(work: fn(usize) -> usize) -> impl FnMut(&usize) -> f64 {
    move |&count| {
        cpu_ms(|| {
            std::hint::black_box(work(count));
        })
    }
}

#[test]
fn a_linear_shape_stays_within_both_bounds() {
    let ratio_run = attempts(
        &[1_000, 4_000],
        5,
        ratio(LINEAR_GROWTH_CAP),
        |n| n,
        shape(linear_work),
    );
    let slope_run = attempts(
        &[1_000, 2_000, 4_000],
        5,
        slope(1.5),
        |n| n,
        shape(linear_work),
    );
    assert!(ratio_run.is_ok(), "{ratio_run:?}");
    assert!(slope_run.is_ok(), "{slope_run:?}");
}

#[test]
fn a_quadratic_shape_breaks_both_bounds_on_every_attempt() {
    let ratio_run = attempts(
        &[1_000, 4_000],
        2,
        ratio(LINEAR_GROWTH_CAP),
        |n| n,
        shape(quadratic_work),
    );
    let slope_run = attempts(
        &[1_000, 2_000, 4_000],
        2,
        slope(1.5),
        |n| n,
        shape(quadratic_work),
    );
    assert_eq!(
        ratio_run.as_ref().map_err(Vec::len),
        Err(ATTEMPTS),
        "{ratio_run:?}"
    );
    assert_eq!(
        slope_run.as_ref().map_err(Vec::len),
        Err(ATTEMPTS),
        "{slope_run:?}"
    );
}

#[test]
fn a_broken_bound_names_each_attempt() {
    let panic = std::panic::catch_unwind(|| {
        assert_bounded(
            "quadratic",
            &[1_000, 4_000],
            1,
            ratio(LINEAR_GROWTH_CAP),
            |n| n,
            shape(quadratic_work),
        );
    })
    .expect_err("a quadratic shape breaks the cap");

    let message = panic.downcast_ref::<String>().expect("a formatted panic");
    assert!(
        message.starts_with("quadratic: every attempt broke the bound"),
        "{message}"
    );
    for attempt in 1..=ATTEMPTS {
        assert!(
            message.contains(&format!("attempt {attempt} [(1000, ")),
            "{message}"
        );
    }
}
