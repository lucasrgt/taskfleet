fn main() {
    let result = taskfleet::run_cli_at(
        std::env::args_os().collect(),
        &std::env::current_dir().unwrap_or_default(),
        &mut std::io::stdin().lock(),
        &mut std::io::stdout(),
    );
    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("taskfleet failed: {error:#}");
            std::process::exit(1);
        }
    }
}
