mod agent;
mod attach;
mod boot;
mod browser;
mod certs;
mod cli;
mod fetch;
mod host;
mod install;
mod local;
mod mux;
mod profile;
mod proto;
mod remote;
mod run;
mod socks;
mod target;
mod util;
mod x11;
mod x11sec;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("agent") => agent::main(&args[2..]),
        Some("open") => agent::open_main(&args[2..]),
        _ => {}
    }
    let cli = <cli::Cli as clap::Parser>::parse();
    let code = match host::run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("rembro: {e}");
            1
        }
    };
    std::process::exit(code);
}
