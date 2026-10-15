fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if let Err(error) = tac_scm_game_runner::cli(&arguments) {
        eprintln!("{error}");
        std::process::exit(2);
    }
}
