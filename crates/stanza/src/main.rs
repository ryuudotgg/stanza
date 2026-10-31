use std::io::{Read, Write};
use std::process::ExitCode;
use std::thread;

use stanza::cli::{Io, run};

// oxc's parser recurses on nested input, so the work runs on a thread with room for the deep files the reference parses.
const STACK_BYTES: usize = 1 << 30;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    let cwd = std::env::current_dir()
        .expect("the working directory is readable")
        .to_string_lossy()
        .into_owned();

    let worker = thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(move || {
            let mut io = Io {
                cwd,
                stdin: Box::new(|| {
                    let mut bytes = Vec::new();
                    std::io::stdin().read_to_end(&mut bytes).map(|_| bytes)
                }),
                stdout: Box::new(std::io::stdout()),
                stderr: Box::new(std::io::stderr()),
            };

            run(&args, &mut io)
        })
        .expect("the work thread starts");

    let code = worker.join().expect("the work thread finishes");
    let _ = std::io::stdout().flush();
    ExitCode::from(code as u8)
}
