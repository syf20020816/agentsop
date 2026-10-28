#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // 无参数或 serve → 打开面板；其余走 CLI（同一二进制，无额外运行时）。
    if let Some(first) = args.first() {
        if first != "serve" {
            std::process::exit(agentsop_lib::cli_entry(args));
        }
    }
    agentsop_lib::run();
}
