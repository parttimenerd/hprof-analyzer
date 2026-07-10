use std::{env, path::PathBuf};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: hprof-analyzer <file.hprof[.gz]> [output.md]");
        std::process::exit(1);
    }
    let input = PathBuf::from(&args[1]);
    let output = args.get(2).map(PathBuf::from);
    eprintln!("Input: {:?}  Output: {:?}", input, output);
}
mod types;
mod reader;
