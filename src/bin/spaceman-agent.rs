fn main() {
    if let Err(error) = spaceman::app::run() {
        eprintln!("Spaceman failed: {error}");
        std::process::exit(1);
    }
}
