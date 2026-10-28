mod app;
mod cli;
mod server;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    app::run();
}

pub fn cli_entry(args: Vec<String>) -> i32 {
    cli::run(args)
}
