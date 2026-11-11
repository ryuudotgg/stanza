use std::mem::MaybeUninit;
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread;

const INTERRUPTS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

const WAITER_STACK: usize = 64 * 1024;

static DEFERRED: OnceLock<libc::sigset_t> = OnceLock::new();

struct Writes {
    in_flight: usize,
    interrupted: bool,
}

static WRITES: Mutex<Writes> = Mutex::new(Writes {
    in_flight: 0,
    interrupted: false,
});

static SETTLED: Condvar = Condvar::new();

fn writes() -> MutexGuard<'static, Writes> {
    WRITES.lock().unwrap_or_else(PoisonError::into_inner)
}

fn wait_while(blocked: impl Fn(&Writes) -> bool) -> MutexGuard<'static, Writes> {
    let mut writes = writes();
    while blocked(&writes) {
        writes = SETTLED.wait(writes).unwrap_or_else(PoisonError::into_inner);
    }

    writes
}

pub struct Writing(());

pub fn writing() -> Writing {
    wait_while(|writes| writes.interrupted).in_flight += 1;
    Writing(())
}

impl Drop for Writing {
    fn drop(&mut self) {
        let mut writes = writes();
        writes.in_flight -= 1;
        if writes.in_flight == 0 {
            SETTLED.notify_all();
        }
    }
}

pub fn exit_unless_interrupted() {
    drop(wait_while(|writes| writes.interrupted));
}

// std spawns with the calling thread's mask, so a child forked while the interrupts are blocked would ignore the Ctrl-C that stops stanza.
pub fn spawn_unblocked<T>(spawn: impl FnOnce() -> T) -> T {
    let Some(deferred) = DEFERRED.get() else {
        return spawn();
    };

    let _settled = wait_while(|writes| writes.interrupted || writes.in_flight > 0);
    unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, deferred, std::ptr::null_mut()) };
    let spawned = spawn();
    unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, deferred, std::ptr::null_mut()) };
    spawned
}

fn die_by(signal: libc::c_int) -> ! {
    writes().interrupted = true;
    #[cfg(debug_assertions)]
    if std::env::var_os("STANZA_PAUSE").is_some() {
        eprintln!("stanza: interrupted");
    }

    let _settled = wait_while(|writes| writes.in_flight > 0);

    let mut set = empty_set();
    unsafe {
        libc::sigaddset(&mut set, signal);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
        libc::raise(signal);
    }

    std::process::exit(128 + signal)
}

fn empty_set() -> libc::sigset_t {
    let mut set = MaybeUninit::uninit();
    unsafe {
        libc::sigemptyset(set.as_mut_ptr());
        set.assume_init()
    }
}

fn ignored(signal: libc::c_int) -> bool {
    let mut action = MaybeUninit::<libc::sigaction>::uninit();
    unsafe {
        libc::sigaction(signal, std::ptr::null(), action.as_mut_ptr()) == 0
            && action.assume_init().sa_sigaction == libc::SIG_IGN
    }
}

pub fn defer_interrupts() {
    let mut started = empty_set();
    if unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &empty_set(), &mut started) } != 0 {
        return;
    }

    // A signal ignored or blocked at start, as under nohup, keeps that state: blocked, Linux would queue an ignored one for sigwait.
    let deferred: Vec<libc::c_int> = INTERRUPTS
        .into_iter()
        .filter(|signal| !ignored(*signal) && unsafe { libc::sigismember(&started, *signal) } == 0)
        .collect();

    if deferred.is_empty() {
        return;
    }

    let mut set = empty_set();
    for signal in deferred {
        unsafe { libc::sigaddset(&mut set, signal) };
    }

    if unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) } != 0 {
        return;
    }

    let waiter = thread::Builder::new()
        .name("stanza signals".to_owned())
        .stack_size(WAITER_STACK)
        .spawn(move || {
            let mut signal = 0;
            loop {
                if unsafe { libc::sigwait(&set, &mut signal) } == 0 {
                    die_by(signal);
                }
            }
        });

    match waiter {
        Ok(_) => {
            let _ = DEFERRED.set(set);
        }
        Err(_) => unsafe {
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
        },
    }
}
