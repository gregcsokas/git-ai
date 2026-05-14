use std::process;

pub fn handle_bg(args: &[String]) {
    match args.first().map(String::as_str) {
        Some("run") => {
            if let Err(e) = git_ai::daemon::run::run_daemon(true) {
                eprintln!("git-ai bg run: {}", e);
                process::exit(1);
            }
        }
        Some("start") => {
            if let Err(e) = git_ai::daemon::run::run_daemon(false) {
                eprintln!("git-ai bg start: {}", e);
                process::exit(1);
            }
        }
        Some("stop") => {
            if let Err(e) = git_ai::daemon::run::stop_daemon() {
                eprintln!("git-ai bg stop: {}", e);
                process::exit(1);
            }
        }
        Some("status") => {
            git_ai::daemon::run::print_status();
        }
        _ => {
            eprintln!("usage: git-ai bg <run|start|stop|status>");
            process::exit(1);
        }
    }
}
