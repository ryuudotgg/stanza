// libtest runs every #[test] on a spawned thread, and thread_stack reads RLIMIT_STACK only on the main thread, so this target has no harness.

#[cfg(target_os = "linux")]
fn main() {
    const LOWERED: libc::rlim_t = 4 << 20;

    let main_thread = unsafe { libc::syscall(libc::SYS_gettid) == libc::getpid() as libc::c_long };
    assert!(main_thread, "the test must run on the main thread");

    let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    let read = unsafe { libc::getrlimit(libc::RLIMIT_STACK, limit.as_mut_ptr()) };
    assert_eq!(read, 0);

    let mut limit = unsafe { limit.assume_init() };
    limit.rlim_cur = LOWERED;
    let lowered = unsafe { libc::setrlimit(libc::RLIMIT_STACK, &limit) };
    assert_eq!(lowered, 0);

    assert_eq!(stanza::pool::thread_stack(), Some(4 << 20));
}

#[cfg(not(target_os = "linux"))]
fn main() {}
