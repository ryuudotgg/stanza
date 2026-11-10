use std::process::ExitCode;

use mimalloc::MiMalloc;
use stanza::cli::{Io, command, execute, working_directory};
use stanza::format::silence_caught_panics;
use stanza::pool::on_largest_stack;

#[global_allocator]
static ALLOCATOR: MiMalloc = MiMalloc;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

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

    ExitCode::from(code as u8)
}
