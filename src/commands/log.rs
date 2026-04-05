use crate::config;
use crate::git::find_repository;

/// Extract recognized Git global flags from args so they can be placed
/// before the `log` subcommand. Everything else passes through to `git log`.
///
/// We deliberately skip the ambiguous short forms `-p` (paginate vs patch),
/// `-P` (no-pager vs perl-regexp), `-C` (change-dir vs copy-detection),
/// and bare `-c` (config vs combined-diff).
/// Their long-form equivalents (`--paginate`, `--no-pager`, `--git-dir`,
/// `--work-tree`, `-c key=val`) are handled correctly.
fn extract_git_global_args(args: &[String]) -> (Vec<String>, Vec<String>) {
    let mut global_args: Vec<String> = Vec::new();
    let mut rest: Vec<String> = Vec::new();
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];

        // --- Global no-value long options (unambiguous with git log) ---
        if matches!(
            arg.as_str(),
            "--paginate"
                | "--no-pager"
                | "--no-replace-objects"
                | "--no-lazy-fetch"
                | "--no-optional-locks"
                | "--no-advice"
                | "--bare"
                | "--literal-pathspecs"
                | "--glob-pathspecs"
                | "--noglob-pathspecs"
                | "--icase-pathspecs"
        ) {
            global_args.push(arg.clone());
            i += 1;
            continue;
        }

        // --- Global takes-value long options: --opt=val or --opt val ---
        if matches!(
            arg.as_str(),
            "--git-dir"
                | "--work-tree"
                | "--namespace"
                | "--config-env"
                | "--list-cmds"
                | "--attr-source"
                | "--super-prefix"
        ) {
            global_args.push(arg.clone());
            if i + 1 < args.len() {
                global_args.push(args[i + 1].clone());
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }

        // --exec-path can be standalone (query) or --exec-path=<path> (set)
        if arg == "--exec-path" {
            global_args.push(arg.clone());
            i += 1;
            continue;
        }

        // =<value> forms for all long takes-value options
        if arg.starts_with("--git-dir=")
            || arg.starts_with("--work-tree=")
            || arg.starts_with("--namespace=")
            || arg.starts_with("--config-env=")
            || arg.starts_with("--list-cmds=")
            || arg.starts_with("--attr-source=")
            || arg.starts_with("--super-prefix=")
            || arg.starts_with("--exec-path=")
        {
            global_args.push(arg.clone());
            i += 1;
            continue;
        }

        // --- Short flags that are unambiguous ---

        // -C is deliberately NOT extracted:
        //   git global: -C <path> (change directory before doing anything)
        //   git log:    -C (detect copies, no argument)
        // Since all args arrive after the `log` keyword is stripped, a bare
        // `-C` is far more likely to be copy-detection. Users needing the
        // global form should use `--git-dir` or `--work-tree` instead.

        // -c <key>=<value>: git config override.
        // Git config keys are always `section.variable=value`, so a valid
        // assignment contains a '.' before the first '='.  A bare `-c`
        // without such a token is git log's combined-diff flag, and a next
        // token like `--format=%H` is a log option (no dot in key portion).
        if arg == "-c"
            && i + 1 < args.len()
            && args[i + 1]
                .find('=')
                .is_some_and(|eq| args[i + 1][..eq].contains('.'))
        {
            global_args.push(arg.clone());
            global_args.push(args[i + 1].clone());
            i += 2;
            continue;
        }

        // -c<key>=<value> sticky form
        if arg.starts_with("-c") && arg.len() > 2 && arg[2..].contains('=') {
            global_args.push(arg.clone());
            i += 1;
            continue;
        }

        // -p, -P, and -C are deliberately NOT extracted:
        //   -p = git log --patch (not --paginate)
        //   -P = git log --perl-regexp (not --no-pager)
        //   -C = git log copy-detection (not --git-dir/change-dir)

        // Everything else (including --help, --version, -h, -v, -p, -P, -C,
        // and all git-log options) passes through to git log.
        rest.push(arg.clone());
        i += 1;
    }

    (global_args, rest)
}

/// Handle the `git ai log` command by proxying to `git log --notes=ai`.
///
/// All additional arguments are forwarded to `git log` as-is, allowing
/// users to filter, format, and paginate the output just like native git log.
pub fn handle_log(args: &[String]) {
    // Separate git-global flags from log arguments.
    let (global_args, log_args) = extract_git_global_args(args);

    // Validate we're inside a git repository, respecting any -C / --git-dir.
    if let Err(e) = find_repository(&global_args) {
        eprintln!("Failed to find repository: {}", e);
        std::process::exit(1);
    }

    // Build: git [global_args] log --notes=ai [log_args...]
    let git_cmd = config::Config::get().git_cmd().to_string();
    let mut cmd = std::process::Command::new(&git_cmd);
    cmd.args(&global_args);
    cmd.arg("log");
    cmd.arg("--notes=ai");
    cmd.args(&log_args);

    // Inherit stdin/stdout/stderr so the pager and terminal colors work
    cmd.stdin(std::process::Stdio::inherit());
    cmd.stdout(std::process::Stdio::inherit());
    cmd.stderr(std::process::Stdio::inherit());

    match cmd.status() {
        Ok(status) => {
            std::process::exit(status.code().unwrap_or(1));
        }
        Err(e) => {
            eprintln!("Failed to execute git log: {}", e);
            std::process::exit(1);
        }
    }
}
