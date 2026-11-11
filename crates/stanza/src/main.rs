use std::ffi::OsString;
use std::process::ExitCode;

use mimalloc::MiMalloc;
use stanza::cli::{Io, command, execute, working_directory};
use stanza::format::silence_caught_panics;
use stanza::pool::on_largest_stack;
use stanza::signals::{defer_interrupts, exit_unless_interrupted};

#[global_allocator]
static ALLOCATOR: MiMalloc = MiMalloc;

fn main() -> ExitCode {
    defer_interrupts();

    let args: Vec<OsString> = std::env::args_os().skip(1).collect();

    let agent_hooks = std::env::var("AGENT_HOOKS").ok();
    let command = command(&args, agent_hooks.as_deref());
    let cwd = working_directory();

    silence_caught_panics();

    let work = |stack| {
        let mut io = Io::system(cwd.clone(), stack, |name| std::env::var_os(name));
        execute(&command, &mut io)
    };

    let code = if command.parses_source() {
        on_largest_stack(work)
    } else {
        work(None)
    };

    exit_unless_interrupted();
    ExitCode::from(code as u8)
}
