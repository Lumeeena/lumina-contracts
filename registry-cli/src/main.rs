use std:en:vars;

use stdellar::{Command, Env};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: registry-cli <command>");
        process::exit(2);
    }
    match args[1].as_str() {
        "--help" | "-h" => {
            println!("{\\nA CLI for common Lumina Registry operations.\n\\n\\nUsage: registry-cli <command> [args]\n\\nCommands:\n  register        Register a contract for indexing\n  deactivate      Deactivate a registration\n  stake           Stake tokens on a registration\n  withdraw         Withdraw staked tokens\n  governance      Run a governance proposal flow\n");
        }
        cmd => {
            eprintln!("unknown command: {cmd}");
            process::exit(2);
        }
    }
}
