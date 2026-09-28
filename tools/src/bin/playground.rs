use std::io;

fn main() -> io::Result<()> {
    vibescript_tools::playground::serve(io::stdin().lock(), io::stdout().lock())
}
