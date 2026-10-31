use std::io::Write;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (stdout, stderr, code) = stanza::cli::run(&args);

    std::io::stdout().write_all(stdout.as_bytes()).unwrap();
    std::io::stderr().write_all(stderr.as_bytes()).unwrap();
    ExitCode::from(code as u8)
}
