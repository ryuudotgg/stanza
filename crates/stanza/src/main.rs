use std::process::ExitCode;
use std::thread;

use stanza::cli::{Io, run};
use stanza::pool::{STACK_BYTES, silence_caught_panics};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    let cwd = std::env::current_dir()
        .expect("the working directory is readable")
        .to_string_lossy()
        .into_owned();

    silence_caught_panics();

    let worker = thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(move || run(&args, &mut Io::system(cwd, |name| std::env::var_os(name))));

    let code = match worker {
        Ok(worker) => worker.join().expect("the work thread finishes"),
        Err(error) => {
            eprintln!("stanza: cannot start the work thread: {error}");
            2
        }
    };

    ExitCode::from(code as u8)
}
