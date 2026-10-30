use mastering_studio::ui;

fn main() {
    println!("============================================================");
    println!("                     Mastering Studio                       ");
    println!("============================================================");
    println!("Starting...");

    if let Err(e) = ui::run_app() {
        eprintln!("Fatal error running Mastering Studio: {}", e);
        std::process::exit(1);
    }

    println!("Mastering Studio exited cleanly.");
}
