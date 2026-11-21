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

const TWO_SIZES: [usize; 2] = [1_000, 4_000];
const THREE_SIZES: [usize; 3] = [1_000, 2_000, 4_000];
const QUADRATIC_OVER_TWO_SIZES: [&[f64]; ATTEMPTS] = [&[1.0, 16.0], &[2.0, 32.0], &[3.0, 48.0]];
const QUADRATIC_OVER_THREE_SIZES: [&[f64]; ATTEMPTS] =
    [&[1.0, 4.0, 16.0], &[2.0, 8.0, 32.0], &[3.0, 12.0, 48.0]];

fn scripted(script: &[&[f64]]) -> impl FnMut(&usize) -> f64 {
    let warm_up = &script[..1];
    let mut samples = warm_up
        .iter()
        .chain(script)
        .flat_map(|attempt| attempt.to_vec())
        .collect::<Vec<f64>>()
        .into_iter();

    move |_| {
        samples
            .next()
            .expect("the script holds every sample the harness takes")
    }
}

fn keyed(sizes: &[usize], samples: &[f64]) -> Attempt<usize> {
    sizes.iter().copied().zip(samples.iter().copied()).collect()
}

fn every_attempt(sizes: &[usize], script: &[&[f64]]) -> Vec<Attempt<usize>> {
    script.iter().map(|samples| keyed(sizes, samples)).collect()
}

#[test]
fn a_linear_shape_stays_within_both_bounds() {
    let ratio_run = attempts(
        &TWO_SIZES,
        1,
        ratio(LINEAR_GROWTH_CAP),
        |n| n,
        scripted(&[&[2.0, 8.0]]),
    );
    let slope_run = attempts(
        &THREE_SIZES,
        1,
        slope(1.5),
        |n| n,
        scripted(&[&[2.0, 4.0, 8.0]]),
    );

    assert_eq!(ratio_run, Ok(keyed(&TWO_SIZES, &[2.0, 8.0])));
    assert_eq!(slope_run, Ok(keyed(&THREE_SIZES, &[2.0, 4.0, 8.0])));
}

#[test]
fn a_quadratic_shape_breaks_both_bounds_on_every_attempt() {
    let ratio_run = attempts(
        &TWO_SIZES,
        1,
        ratio(LINEAR_GROWTH_CAP),
        |n| n,
        scripted(&QUADRATIC_OVER_TWO_SIZES),
    );
    let slope_run = attempts(
        &THREE_SIZES,
        1,
        slope(1.5),
        |n| n,
        scripted(&QUADRATIC_OVER_THREE_SIZES),
    );

    assert_eq!(
        ratio_run,
        Err(every_attempt(&TWO_SIZES, &QUADRATIC_OVER_TWO_SIZES))
    );
    assert_eq!(
        slope_run,
        Err(every_attempt(&THREE_SIZES, &QUADRATIC_OVER_THREE_SIZES))
    );
}

#[test]
fn a_bound_that_holds_on_the_third_attempt_passes() {
    let ratio_run = attempts(
        &TWO_SIZES,
        1,
        ratio(LINEAR_GROWTH_CAP),
        |n| n,
        scripted(&[&[1.0, 16.0], &[2.0, 32.0], &[3.0, 12.0]]),
    );
    let slope_run = attempts(
        &THREE_SIZES,
        1,
        slope(1.5),
        |n| n,
        scripted(&[&[1.0, 4.0, 16.0], &[2.0, 8.0, 32.0], &[3.0, 6.0, 12.0]]),
    );

    assert_eq!(ratio_run, Ok(keyed(&TWO_SIZES, &[3.0, 12.0])));
    assert_eq!(slope_run, Ok(keyed(&THREE_SIZES, &[3.0, 6.0, 12.0])));
}

#[test]
fn a_broken_bound_names_each_attempt() {
    let panic = std::panic::catch_unwind(|| {
        assert_bounded(
            "quadratic",
            &TWO_SIZES,
            1,
            ratio(LINEAR_GROWTH_CAP),
            |n| n,
            scripted(&QUADRATIC_OVER_TWO_SIZES),
        );
    })
    .expect_err("a quadratic shape breaks the cap");

    let message = panic.downcast_ref::<String>().expect("a formatted panic");
    assert!(
        message.starts_with("quadratic: every attempt broke the bound"),
        "{message}"
    );
    for attempt in [
        "attempt 1 [(1000, 1.0), (4000, 16.0)]",
        "attempt 2 [(1000, 2.0), (4000, 32.0)]",
        "attempt 3 [(1000, 3.0), (4000, 48.0)]",
    ] {
        assert!(message.contains(attempt), "{message}");
    }
}

#[test]
fn busy_work_reads_cpu_time() {
    let spent = cpu_ms(|| {
        std::hint::black_box((0..1_000_000_usize).fold(0_usize, |sum, item| {
            sum.wrapping_add(std::hint::black_box(item))
        }));
    });

    assert!(spent > 0.0, "{spent}");
}
