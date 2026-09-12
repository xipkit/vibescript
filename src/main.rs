use std::{
    fs,
    io::{self, Write},
    process::ExitCode,
    time::{Duration, Instant},
};
use vibescript::{CallOptions, Engine, parse_json, stringify_json};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut file = None;
    let mut function = None;
    let mut input = Vec::new();
    let mut options = CallOptions::default();
    let mut stats = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!(
                    "Usage: vibes FILE [--function NAME] [--arg JSON] [--steps N] [--memory N] [--recursion N] [--timeout-ms N] [--stats]\nZero steps or memory disables that quota. Results are printed as JSON."
                );
                return Ok(());
            }
            "--version" => {
                println!("vibescript.rs {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--function" => function = Some(args.next().ok_or("missing function name")?),
            "--arg" => {
                let raw = args.next().ok_or("missing JSON argument")?;
                input.push(parse_json(raw.as_bytes(), CallOptions::default())?.value);
            }
            "--steps" => {
                let n = args.next().ok_or("missing step quota")?.parse()?;
                options.limits.steps = if n == 0 { None } else { Some(n) };
            }
            "--memory" => {
                let n = args.next().ok_or("missing memory quota")?.parse()?;
                options.limits.memory_bytes = if n == 0 { None } else { Some(n) };
            }
            "--recursion" => {
                options.limits.recursion = args.next().ok_or("missing recursion limit")?.parse()?
            }
            "--timeout-ms" => {
                let n = args.next().ok_or("missing timeout")?.parse()?;
                options.deadline = Instant::now().checked_add(Duration::from_millis(n));
                if options.deadline.is_none() {
                    return Err("timeout outside supported range".into());
                }
            }
            "--stats" => stats = true,
            _ if arg.starts_with('-') => return Err(format!("unknown option {arg}").into()),
            _ => {
                if file.replace(arg).is_some() {
                    return Err("expected one source file".into());
                }
            }
        }
    }
    let file = file.ok_or("expected source file; use --help")?;
    let source = fs::read_to_string(file)?;
    let script = Engine::new().compile(&source)?;
    let result = if let Some(name) = function {
        script.call(&name, &input, options)?
    } else {
        if !input.is_empty() {
            return Err("--arg requires --function".into());
        }
        script.run(options)?
    };
    let encoded = stringify_json(&result.value, CallOptions::default())?;
    let mut out = io::stdout().lock();
    out.write_all(encoded.value.as_bytes().unwrap())?;
    out.write_all(b"\n")?;
    if stats {
        eprintln!(
            "steps={} peak_bytes={} retained_bytes={}",
            result.stats.steps, result.stats.peak_memory_bytes, result.stats.retained_memory_bytes
        );
    }
    Ok(())
}
