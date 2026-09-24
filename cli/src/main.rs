mod cli;
mod repl;

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1).peekable();
    if args.next_if(|arg| arg == "repl").is_some() {
        return repl::run(args);
    }
    let command = match cli::parse(args) {
        Ok(command) => command,
        Err(failure) => return report(failure),
    };
    let result = match command {
        cli::Command::Help(text) => {
            print!("{text}");
            Ok(())
        }
        cli::Command::Version => {
            println!("vibescript.rs {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        cli::Command::Run(invocation) => cli::run(*invocation),
        cli::Command::Check(analysis) => cli::check(*analysis),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => report(failure),
    }
}

fn report(failure: cli::Failure) -> ExitCode {
    eprintln!("{failure}");
    failure.exit_code()
}
