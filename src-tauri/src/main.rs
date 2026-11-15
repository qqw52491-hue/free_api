#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() == Some(std::ffi::OsStr::new(tauri_app_lib::shell_receipt::MODE)) {
        let (Some(path), Some(script), None) = (args.next(), args.next(), args.next()) else {
            std::process::exit(2);
        };
        let code = tauri_app_lib::shell_receipt::prepare_supervisor_signals()
            .and_then(|_| tauri_app_lib::shell_receipt::supervise(std::path::Path::new(&path), &script))
            .unwrap_or_else(|error| { eprintln!("Shell supervisor: {error}"); 1 });
        std::process::exit(code);
    }
    tauri_app_lib::run()
}
