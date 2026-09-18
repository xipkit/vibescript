mod cli;

use std::process::ExitCode;

fn main() -> ExitCode {
    let command = match cli::parse(std::env::args_os().skip(1)) {
        Ok(command) => command,
        Err(failure) => return report(failure),
    };
    let result = match command {
        cli::Command::Help => {
            print!("{}", cli::HELP);
            Ok(())
        }
        cli::Command::Version => {
            println!("vibescript.rs {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        cli::Command::Run(invocation) => cli::run(*invocation),
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
