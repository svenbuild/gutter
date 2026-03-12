fn main() {
    if let Err(error) = gutter::run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
