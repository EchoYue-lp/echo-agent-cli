fn main() {
    if let Err(error) = echo_agent_cli::chrome_native_host::run() {
        eprintln!("EKO Chrome native host stopped: {error}");
        std::process::exit(1);
    }
}
