use std::path::PathBuf;

/// A parsed trace2 event relevant to git-ai.
#[derive(Debug, Clone)]
pub enum Trace2Event {
    /// A git command started (carries argv so we can extract cmd_name)
    Start { sid: String, argv: Vec<String> },
    /// Repository path discovered from def_repo event
    DefRepo { sid: String, repo_path: PathBuf },
    /// The command name was identified (e.g. "commit", "push", "rebase")
    CmdName { sid: String, cmd_name: String },
    /// A git command completed
    CommandExit { sid: String, exit_code: i32 },
    /// Parsed but not actionable
    Ignored,
}

/// Extract the root session ID from a potentially nested sid.
///
/// Trace2 session IDs for child processes are formatted as `parent_sid/child_sid`.
/// This returns the first segment (the root), which identifies the top-level git process.
pub fn root_sid(sid: &str) -> &str {
    sid.split('/').next().unwrap_or(sid)
}

/// Returns true if this sid represents a root-level (non-child) process.
pub fn is_root_sid(sid: &str) -> bool {
    !sid.contains('/')
}

/// Parse a single trace2 JSON line into a Trace2Event.
/// Returns None if the line is malformed or empty.
pub fn parse_trace2_line(line: &str) -> Option<Trace2Event> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let obj = value.as_object()?;

    let event = obj.get("event")?.as_str()?;
    let sid = obj.get("sid")?.as_str()?.to_string();

    match event {
        "start" => {
            let argv = obj
                .get("argv")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            Some(Trace2Event::Start { sid, argv })
        }
        "def_repo" => {
            let worktree = obj.get("worktree")?.as_str()?;
            Some(Trace2Event::DefRepo {
                sid,
                repo_path: PathBuf::from(worktree),
            })
        }
        "cmd_name" => {
            let name = obj.get("name")?.as_str()?.to_string();
            Some(Trace2Event::CmdName {
                sid,
                cmd_name: name,
            })
        }
        "exit" | "atexit" => {
            let code = obj.get("code").and_then(|v| v.as_i64()).unwrap_or(-1) as i32;
            Some(Trace2Event::CommandExit {
                sid,
                exit_code: code,
            })
        }
        _ => Some(Trace2Event::Ignored),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_start_event() {
        let line = r#"{"event":"start","sid":"1234567890-abcdef","thread":"main","time":"2024-01-01T00:00:00Z","argv":["git","commit","-m","test"]}"#;
        let event = parse_trace2_line(line).unwrap();
        match event {
            Trace2Event::Start { sid, argv } => {
                assert_eq!(sid, "1234567890-abcdef");
                assert_eq!(argv, vec!["git", "commit", "-m", "test"]);
            }
            _ => panic!("expected Start event"),
        }
    }

    #[test]
    fn parse_def_repo_event() {
        let line = r#"{"event":"def_repo","sid":"1234567890-abcdef","thread":"main","repo":1,"worktree":"/home/user/project"}"#;
        let event = parse_trace2_line(line).unwrap();
        match event {
            Trace2Event::DefRepo { sid, repo_path } => {
                assert_eq!(sid, "1234567890-abcdef");
                assert_eq!(repo_path, PathBuf::from("/home/user/project"));
            }
            _ => panic!("expected DefRepo event"),
        }
    }

    #[test]
    fn parse_cmd_name_event() {
        let line =
            r#"{"event":"cmd_name","sid":"1234567890-abcdef","thread":"main","name":"commit"}"#;
        let event = parse_trace2_line(line).unwrap();
        match event {
            Trace2Event::CmdName { sid, cmd_name } => {
                assert_eq!(sid, "1234567890-abcdef");
                assert_eq!(cmd_name, "commit");
            }
            _ => panic!("expected CmdName event"),
        }
    }

    #[test]
    fn parse_exit_event() {
        let line =
            r#"{"event":"exit","sid":"1234567890-abcdef","thread":"main","t_abs":0.05,"code":0}"#;
        let event = parse_trace2_line(line).unwrap();
        match event {
            Trace2Event::CommandExit { sid, exit_code } => {
                assert_eq!(sid, "1234567890-abcdef");
                assert_eq!(exit_code, 0);
            }
            _ => panic!("expected CommandExit event"),
        }
    }

    #[test]
    fn parse_atexit_event() {
        let line =
            r#"{"event":"atexit","sid":"1234567890-abcdef","thread":"main","t_abs":0.05,"code":1}"#;
        let event = parse_trace2_line(line).unwrap();
        match event {
            Trace2Event::CommandExit { sid, exit_code } => {
                assert_eq!(sid, "1234567890-abcdef");
                assert_eq!(exit_code, 1);
            }
            _ => panic!("expected CommandExit event"),
        }
    }

    #[test]
    fn parse_ignored_event() {
        let line =
            r#"{"event":"data","sid":"abc","thread":"main","key":"some_key","value":"some_value"}"#;
        let event = parse_trace2_line(line).unwrap();
        assert!(matches!(event, Trace2Event::Ignored));
    }

    #[test]
    fn parse_empty_line_returns_none() {
        assert!(parse_trace2_line("").is_none());
        assert!(parse_trace2_line("   ").is_none());
    }

    #[test]
    fn parse_malformed_json_returns_none() {
        assert!(parse_trace2_line("not json at all").is_none());
        assert!(parse_trace2_line("{incomplete").is_none());
    }

    #[test]
    fn parse_missing_required_fields_returns_none() {
        // Missing "event" field
        let line = r#"{"sid":"abc","thread":"main"}"#;
        assert!(parse_trace2_line(line).is_none());

        // Missing "sid" field
        let line = r#"{"event":"start","thread":"main","argv":["git","status"]}"#;
        assert!(parse_trace2_line(line).is_none());
    }

    #[test]
    fn root_sid_extraction() {
        assert_eq!(root_sid("1234567890-abcdef"), "1234567890-abcdef");
        assert_eq!(
            root_sid("1234567890-abcdef/9876543210-fedcba"),
            "1234567890-abcdef"
        );
        assert_eq!(root_sid("a/b/c"), "a");
    }

    #[test]
    fn is_root_sid_check() {
        assert!(is_root_sid("1234567890-abcdef"));
        assert!(!is_root_sid("1234567890-abcdef/child"));
        assert!(!is_root_sid("a/b/c"));
    }

    #[test]
    fn parse_exit_with_missing_code_defaults_to_negative() {
        let line = r#"{"event":"exit","sid":"abc","thread":"main","t_abs":0.05}"#;
        let event = parse_trace2_line(line).unwrap();
        match event {
            Trace2Event::CommandExit { exit_code, .. } => {
                assert_eq!(exit_code, -1);
            }
            _ => panic!("expected CommandExit event"),
        }
    }

    #[test]
    fn parse_start_with_no_argv() {
        let line = r#"{"event":"start","sid":"abc","thread":"main","time":"2024-01-01T00:00:00Z"}"#;
        let event = parse_trace2_line(line).unwrap();
        match event {
            Trace2Event::Start { argv, .. } => {
                assert!(argv.is_empty());
            }
            _ => panic!("expected Start event"),
        }
    }
}
