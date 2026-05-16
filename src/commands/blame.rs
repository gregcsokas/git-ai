use git_ai::core::authorship_log::AuthorshipLog;

use std::collections::HashMap;
use std::process;

use crate::commands::helpers::git_cmd;

pub fn handle_blame(args: &[String]) {
    if args.is_empty() {
        eprintln!("usage: git-ai blame <file>");
        process::exit(1);
    }

    // Detect output mode flags (git-ai specific, not passed to git)
    #[derive(PartialEq)]
    enum BlameOutputMode {
        Default,
        Porcelain,
        LinePorcelain,
        Incremental,
        Json,
    }

    let mut output_mode = BlameOutputMode::Default;
    let mut blame_flags: Vec<String> = Vec::new();
    let mut file_path_arg: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--json" {
            output_mode = BlameOutputMode::Json;
            i += 1;
        } else if args[i] == "--porcelain" {
            output_mode = BlameOutputMode::Porcelain;
            i += 1;
        } else if args[i] == "--line-porcelain" {
            output_mode = BlameOutputMode::LinePorcelain;
            i += 1;
        } else if args[i] == "--incremental" {
            output_mode = BlameOutputMode::Incremental;
            i += 1;
        } else if args[i] == "-L" {
            if i + 1 < args.len() {
                blame_flags.push(args[i].clone());
                blame_flags.push(args[i + 1].clone());
                i += 2;
            } else {
                eprintln!("git-ai blame: -L requires a range argument");
                process::exit(1);
            }
        } else if args[i].starts_with('-') {
            blame_flags.push(args[i].clone());
            i += 1;
        } else {
            file_path_arg = Some(args[i].clone());
            i += 1;
        }
    }

    let file_path = match file_path_arg {
        Some(p) => p,
        None => {
            eprintln!("usage: git-ai blame <file>");
            process::exit(1);
        }
    };

    // Resolve the file path to repo-relative for authorship note lookups.
    // git blame resolves from cwd, but authorship notes store paths relative to repo root.
    let repo_relative_file_path = {
        let prefix = git_cmd(&["rev-parse", "--show-prefix"]).unwrap_or_default();
        let candidate = if prefix.is_empty() {
            file_path.clone()
        } else {
            format!("{}{}", prefix, file_path)
        };
        // Normalize: resolve .. and . components
        let p = std::path::PathBuf::from(&candidate);
        let mut components: Vec<String> = Vec::new();
        for comp in p.components() {
            match comp {
                std::path::Component::ParentDir => {
                    components.pop();
                }
                std::path::Component::CurDir => {}
                std::path::Component::Normal(s) => {
                    components.push(s.to_string_lossy().to_string());
                }
                _ => {}
            }
        }
        components.join("/")
    };

    // Build the git blame command (always use --line-porcelain for parsing)
    let mut blame_args: Vec<&str> = vec!["blame", "--line-porcelain"];
    for flag in &blame_flags {
        blame_args.push(flag.as_str());
    }
    blame_args.push("--");
    blame_args.push(&file_path);

    let blame_output = match git_cmd(&blame_args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("git-ai blame: {}", e);
            process::exit(1);
        }
    };

    let mut lines: Vec<BlameLineData> = Vec::new();
    let mut cur_sha = String::new();
    let mut cur_orig_line: u32 = 0;
    let mut cur_final_line: u32 = 0;
    let mut cur_author = String::new();
    let mut cur_author_email = String::new();
    let mut cur_author_time: i64 = 0;
    let mut cur_author_tz = String::new();
    let mut cur_headers: Vec<String> = Vec::new();

    for line in blame_output.lines() {
        if line.is_empty() {
            continue;
        }
        if let Some(content) = line.strip_prefix('\t') {
            lines.push(BlameLineData {
                commit_sha: cur_sha.clone(),
                orig_line: cur_orig_line,
                final_line: cur_final_line,
                author: cur_author.clone(),
                author_email: cur_author_email.clone(),
                author_time: cur_author_time,
                author_tz: cur_author_tz.clone(),
                content: content.to_string(),
                raw_headers: cur_headers.clone(),
            });
            cur_headers.clear();
            continue;
        }
        if let Some(rest) = line.strip_prefix("author-mail ") {
            cur_author_email = rest
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string();
            cur_headers.push(line.to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("author-time ") {
            cur_author_time = rest.trim().parse().unwrap_or(0);
            cur_headers.push(line.to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("author-tz ") {
            cur_author_tz = rest.trim().to_string();
            cur_headers.push(line.to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("author ") {
            cur_author = rest.to_string();
            cur_headers.push(line.to_string());
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3
            && parts[0].len() == 40
            && parts[0].chars().all(|c| c.is_ascii_hexdigit())
        {
            cur_sha = parts[0].to_string();
            cur_orig_line = parts[1].parse().unwrap_or(0);
            cur_final_line = parts[2].parse().unwrap_or(0);
            cur_headers.push(line.to_string());
        } else {
            cur_headers.push(line.to_string());
        }
    }

    let mut commit_notes: HashMap<String, Option<AuthorshipLog>> = HashMap::new();
    for blame_line in &lines {
        if !commit_notes.contains_key(&blame_line.commit_sha) {
            let note = load_authorship_note(&blame_line.commit_sha);
            commit_notes.insert(blame_line.commit_sha.clone(), note);
        }
    }

    match output_mode {
        BlameOutputMode::Json => {
            blame_output_json(&lines, &repo_relative_file_path, &commit_notes);
        }
        BlameOutputMode::Porcelain
        | BlameOutputMode::LinePorcelain
        | BlameOutputMode::Incremental => {
            blame_output_porcelain(&lines, &repo_relative_file_path, &commit_notes);
        }
        BlameOutputMode::Default => {
            blame_output_default(&lines, &repo_relative_file_path, &commit_notes);
        }
    }
}

/// Detect if an author email belongs to a known AI agent.
pub fn detect_agent_from_email(email: &str) -> Option<&'static str> {
    let email_lower = email.to_lowercase();
    if email_lower == "noreply@anthropic.com" {
        return Some("claude");
    }
    if email_lower == "noreply@openai.com" {
        return Some("codex");
    }
    if email_lower.contains("copilot") {
        return Some("github-copilot");
    }
    if email_lower.contains("devin") {
        return Some("devin");
    }
    if email_lower.ends_with("@cursor.com") {
        return Some("cursor");
    }
    None
}

pub struct BlameLineData {
    pub commit_sha: String,
    pub orig_line: u32,
    pub final_line: u32,
    pub author: String,
    pub author_email: String,
    pub author_time: i64,
    pub author_tz: String,
    pub content: String,
    pub raw_headers: Vec<String>,
}

pub fn resolve_line_author(
    commit_sha: &str,
    orig_line: u32,
    git_author: &str,
    author_email: &str,
    file_path: &str,
    commit_notes: &HashMap<String, Option<AuthorshipLog>>,
    raw_headers: &[String],
) -> String {
    let (author, _) = resolve_line_author_with_prompt(
        commit_sha,
        orig_line,
        git_author,
        author_email,
        file_path,
        commit_notes,
        raw_headers,
    );
    author
}

pub fn resolve_line_author_with_prompt(
    commit_sha: &str,
    orig_line: u32,
    git_author: &str,
    author_email: &str,
    file_path: &str,
    commit_notes: &HashMap<String, Option<AuthorshipLog>>,
    raw_headers: &[String],
) -> (String, Option<String>) {
    if let Some(Some(authorship_log)) = commit_notes.get(commit_sha) {
        // Extract the original filename from blame porcelain headers (handles renames)
        let orig_filename: Option<&str> =
            raw_headers.iter().find_map(|h| h.strip_prefix("filename "));

        for file_attest in &authorship_log.attestations {
            let attest_path = file_attest
                .file_path
                .strip_prefix("./")
                .unwrap_or(&file_attest.file_path);
            let query_path = file_path.strip_prefix("./").unwrap_or(file_path);
            // Match against the queried file path OR the original filename from blame
            let matches = attest_path == query_path
                || orig_filename.is_some_and(|orig| {
                    let orig_clean = orig.strip_prefix("./").unwrap_or(orig);
                    attest_path == orig_clean
                });
            if !matches {
                continue;
            }
            for entry in &file_attest.entries {
                let covers_line = entry.line_ranges.iter().any(|r| r.contains(orig_line));
                if !covers_line {
                    continue;
                }
                if let Some(prompt) = authorship_log.metadata.prompts.get(&entry.hash) {
                    return (prompt.agent_id.tool.clone(), Some(entry.hash.clone()));
                }
                if entry.hash.starts_with("h_") {
                    return (git_author.to_string(), None);
                }
                if entry.hash.starts_with("s_") {
                    let session_key = entry.hash.split("::").next().unwrap_or(&entry.hash);
                    if let Some(session) = authorship_log.metadata.sessions.get(session_key) {
                        return (session.agent_id.tool.clone(), Some(entry.hash.clone()));
                    }
                }
            }
        }
    }
    if let Some(agent_name) = detect_agent_from_email(author_email) {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(commit_sha.as_bytes());
        hasher.update(b"_agent_email_");
        hasher.update(author_email.as_bytes());
        let hash_bytes = hasher.finalize();
        let prompt_hash = format!("{:x}", hash_bytes)
            .chars()
            .take(16)
            .collect::<String>();
        return (agent_name.to_string(), Some(prompt_hash));
    }
    (git_author.to_string(), None)
}

fn blame_output_default(
    lines: &[BlameLineData],
    file_path: &str,
    commit_notes: &HashMap<String, Option<AuthorshipLog>>,
) {
    let line_num_width = lines.len().to_string().len();
    let mut max_author_width = 0;
    for bl in lines {
        let a = resolve_line_author(
            &bl.commit_sha,
            bl.orig_line,
            &bl.author,
            &bl.author_email,
            file_path,
            commit_notes,
            &bl.raw_headers,
        );
        max_author_width = max_author_width.max(a.len());
    }
    for bl in lines {
        let short_sha = &bl.commit_sha[..7.min(bl.commit_sha.len())];
        let display_author = resolve_line_author(
            &bl.commit_sha,
            bl.orig_line,
            &bl.author,
            &bl.author_email,
            file_path,
            commit_notes,
            &bl.raw_headers,
        );
        let date_str = format_blame_date(bl.author_time, &bl.author_tz);
        println!(
            "{} ({:<width$} {} {:>lwidth$}) {}",
            short_sha,
            display_author,
            date_str,
            bl.final_line,
            bl.content,
            width = max_author_width,
            lwidth = line_num_width
        );
    }
}

fn blame_output_porcelain(
    lines: &[BlameLineData],
    file_path: &str,
    commit_notes: &HashMap<String, Option<AuthorshipLog>>,
) {
    for bl in lines {
        let display_author = resolve_line_author(
            &bl.commit_sha,
            bl.orig_line,
            &bl.author,
            &bl.author_email,
            file_path,
            commit_notes,
            &bl.raw_headers,
        );
        for header in &bl.raw_headers {
            if header.starts_with("author ") && !header.starts_with("author-") {
                println!("author {}", display_author);
            } else {
                println!("{}", header);
            }
        }
        println!("\t{}", bl.content);
    }
}

fn blame_output_json(
    lines: &[BlameLineData],
    file_path: &str,
    commit_notes: &HashMap<String, Option<AuthorshipLog>>,
) {
    use std::collections::BTreeMap;
    let mut line_authors: BTreeMap<u32, String> = BTreeMap::new();
    let mut prompts: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();

    for bl in lines {
        let (author_display, prompt_hash) = resolve_line_author_with_prompt(
            &bl.commit_sha,
            bl.orig_line,
            &bl.author,
            &bl.author_email,
            file_path,
            commit_notes,
            &bl.raw_headers,
        );
        if let Some(hash) = &prompt_hash {
            line_authors.insert(bl.final_line, hash.clone());
            if !prompts.contains_key(hash) {
                if let Some(Some(log)) = commit_notes.get(&bl.commit_sha)
                    && let Some(prompt) = log.metadata.prompts.get(hash)
                {
                    prompts.insert(hash.clone(), serde_json::json!({
                            "agent_id": { "tool": prompt.agent_id.tool, "model": prompt.agent_id.model, "id": prompt.agent_id.id },
                            "accepted_lines": prompt.accepted_lines,
                            "total_additions": prompt.total_additions,
                            "overriden_lines": prompt.overriden_lines,
                            "total_deletions": prompt.total_deletions,
                        }));
                }
                if !prompts.contains_key(hash)
                    && let Some(agent_name) = detect_agent_from_email(&bl.author_email)
                {
                    let total_lines = lines
                        .iter()
                        .filter(|l| l.commit_sha == bl.commit_sha)
                        .count() as u64;
                    let tool_name = format!("{}-agent", agent_name.replace("github-", ""));
                    prompts.insert(hash.clone(), serde_json::json!({
                            "agent_id": { "tool": tool_name, "model": "unknown", "id": bl.commit_sha },
                            "accepted_lines": total_lines,
                            "total_additions": total_lines,
                            "overriden_lines": 0u64,
                            "total_deletions": 0u64,
                        }));
                }
            }
        } else {
            line_authors.insert(bl.final_line, author_display);
        }
    }

    let mut lines_map: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    let entries: Vec<(u32, &String)> = line_authors.iter().map(|(k, v)| (*k, v)).collect();
    if !entries.is_empty() {
        let mut range_start = entries[0].0;
        let mut range_end = entries[0].0;
        let mut range_author = entries[0].1;
        for entry in entries.iter().skip(1) {
            if entry.1 == range_author && entry.0 == range_end + 1 {
                range_end = entry.0;
            } else {
                let key = if range_start == range_end {
                    format!("{}", range_start)
                } else {
                    format!("{}-{}", range_start, range_end)
                };
                lines_map.insert(key, serde_json::Value::String(range_author.clone()));
                range_start = entry.0;
                range_end = entry.0;
                range_author = entry.1;
            }
        }
        let key = if range_start == range_end {
            format!("{}", range_start)
        } else {
            format!("{}-{}", range_start, range_end)
        };
        lines_map.insert(key, serde_json::Value::String(range_author.clone()));
    }

    let output = serde_json::json!({ "lines": lines_map, "prompts": prompts });
    println!(
        "{}",
        serde_json::to_string_pretty(&output).unwrap_or_default()
    );
}

pub fn load_authorship_note(commit_sha: &str) -> Option<AuthorshipLog> {
    let note_content = git_cmd(&["notes", "--ref=ai", "show", commit_sha]).ok()?;
    AuthorshipLog::deserialize_from_string(&note_content).ok()
}

fn format_blame_date(author_time: i64, author_tz: &str) -> String {
    let offset_secs: i64 = if author_tz.len() == 5 {
        let sign: i64 = if author_tz.starts_with('+') { 1 } else { -1 };
        let hours: i64 = author_tz[1..3].parse().unwrap_or(0);
        let mins: i64 = author_tz[3..5].parse().unwrap_or(0);
        sign * (hours * 3600 + mins * 60)
    } else {
        0
    };

    let local_time = author_time + offset_secs;
    let days_since_epoch = local_time.div_euclid(86400);
    let time_of_day = local_time.rem_euclid(86400);

    let hours = time_of_day / 3600;
    let mins = (time_of_day % 3600) / 60;
    let secs = time_of_day % 60;

    let (year, month, day) = days_to_ymd(days_since_epoch);

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} {}",
        year, month, day, hours, mins, secs, author_tz
    )
}

fn days_to_ymd(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use git_ai::core::authorship_log::{
        AgentId, AttestationEntry, AuthorshipLog, FileAttestation, LineRange, Metadata,
        PromptRecord, SessionRecord,
    };
    use std::collections::BTreeMap;

    // =========================================================================
    // detect_agent_from_email tests
    // =========================================================================

    #[test]
    fn detect_agent_anthropic() {
        assert_eq!(
            detect_agent_from_email("noreply@anthropic.com"),
            Some("claude")
        );
    }

    #[test]
    fn detect_agent_openai() {
        assert_eq!(detect_agent_from_email("noreply@openai.com"), Some("codex"));
    }

    #[test]
    fn detect_agent_case_insensitive() {
        assert_eq!(
            detect_agent_from_email("NOREPLY@ANTHROPIC.COM"),
            Some("claude")
        );
        assert_eq!(detect_agent_from_email("Noreply@OpenAI.com"), Some("codex"));
    }

    #[test]
    fn detect_agent_copilot() {
        assert_eq!(
            detect_agent_from_email("copilot@github.com"),
            Some("github-copilot")
        );
        assert_eq!(
            detect_agent_from_email("github-copilot@users.noreply.github.com"),
            Some("github-copilot")
        );
    }

    #[test]
    fn detect_agent_cursor() {
        assert_eq!(detect_agent_from_email("user@cursor.com"), Some("cursor"));
        assert_eq!(
            detect_agent_from_email("noreply@cursor.com"),
            Some("cursor")
        );
    }

    #[test]
    fn detect_agent_devin() {
        assert_eq!(detect_agent_from_email("devin@cognition.ai"), Some("devin"));
    }

    #[test]
    fn detect_agent_human_email_returns_none() {
        assert_eq!(detect_agent_from_email("human@example.com"), None);
        assert_eq!(detect_agent_from_email("developer@gmail.com"), None);
        assert_eq!(detect_agent_from_email(""), None);
    }

    // =========================================================================
    // format_blame_date tests
    // =========================================================================

    #[test]
    fn format_blame_date_known_timestamp() {
        // 2021-01-01 00:00:00 UTC = 1609459200
        let result = format_blame_date(1609459200, "+0000");
        assert_eq!(result, "2021-01-01 00:00:00 +0000");
    }

    #[test]
    fn format_blame_date_positive_timezone() {
        // 1609459200 UTC = 2021-01-01 00:00:00 UTC
        // With +0530 offset, local time = 2021-01-01 05:30:00
        let result = format_blame_date(1609459200, "+0530");
        assert_eq!(result, "2021-01-01 05:30:00 +0530");
    }

    #[test]
    fn format_blame_date_negative_timezone() {
        // 1609459200 UTC = 2021-01-01 00:00:00 UTC
        // With -0500 offset, local time = 2020-12-31 19:00:00
        let result = format_blame_date(1609459200, "-0500");
        assert_eq!(result, "2020-12-31 19:00:00 -0500");
    }

    #[test]
    fn format_blame_date_epoch() {
        let result = format_blame_date(0, "+0000");
        assert_eq!(result, "1970-01-01 00:00:00 +0000");
    }

    // =========================================================================
    // days_to_ymd tests
    // =========================================================================

    #[test]
    fn days_to_ymd_epoch() {
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
    }

    #[test]
    fn days_to_ymd_known_date_2021() {
        // 2021-01-01 is day 18628 since epoch
        assert_eq!(days_to_ymd(18628), (2021, 1, 1));
    }

    #[test]
    fn days_to_ymd_leap_year_feb_29() {
        // 2020-02-29: 2020 is a leap year
        // Days from epoch to 2020-02-29:
        // From 1970-01-01 to 2020-02-29
        // 50 years of days minus adjustments... let's compute:
        // 2020-01-01 is day 18262, Feb has 29 days in 2020, so Feb 29 = 18262 + 31 + 28 = 18321
        // Actually: Jan=31 days, so Feb 1 = 18262+31=18293, Feb 29 = 18293+28=18321
        assert_eq!(days_to_ymd(18321), (2020, 2, 29));
    }

    #[test]
    fn days_to_ymd_known_date_2000() {
        // 2000-01-01 is day 10957 since epoch
        assert_eq!(days_to_ymd(10957), (2000, 1, 1));
    }

    #[test]
    fn days_to_ymd_negative_days() {
        // Day -1 from epoch = 1969-12-31
        assert_eq!(days_to_ymd(-1), (1969, 12, 31));
    }

    // =========================================================================
    // resolve_line_author_with_prompt tests
    // =========================================================================

    fn make_test_authorship_log_with_prompt() -> AuthorshipLog {
        let mut prompts = BTreeMap::new();
        prompts.insert(
            "abc123def4567890".to_string(),
            PromptRecord {
                agent_id: AgentId {
                    tool: "cursor".to_string(),
                    id: "sess_1".to_string(),
                    model: "claude-3-sonnet".to_string(),
                },
                human_author: None,
                messages_url: None,
                total_additions: 10,
                total_deletions: 2,
                accepted_lines: 8,
                overriden_lines: 0,
                custom_attributes: None,
            },
        );

        let metadata = Metadata {
            schema_version: "authorship/3.0.0".to_string(),
            git_ai_version: Some("development".to_string()),
            base_commit_sha: "base123".to_string(),
            prompts,
            sessions: BTreeMap::new(),
            humans: BTreeMap::new(),
        };

        AuthorshipLog {
            attestations: vec![FileAttestation {
                file_path: "src/main.rs".to_string(),
                entries: vec![AttestationEntry {
                    hash: "abc123def4567890".to_string(),
                    line_ranges: vec![LineRange::Range(1, 20)],
                }],
            }],
            metadata,
        }
    }

    #[test]
    fn resolve_line_author_with_prompt_matching_file_and_line() {
        let log = make_test_authorship_log_with_prompt();
        let mut commit_notes: HashMap<String, Option<AuthorshipLog>> = HashMap::new();
        commit_notes.insert("commit_abc".to_string(), Some(log));

        let (author, prompt_hash) = resolve_line_author_with_prompt(
            "commit_abc",
            5,
            "Human Author",
            "human@example.com",
            "src/main.rs",
            &commit_notes,
            &[],
        );

        assert_eq!(author, "cursor");
        assert_eq!(prompt_hash, Some("abc123def4567890".to_string()));
    }

    #[test]
    fn resolve_line_author_with_prompt_human_hash() {
        let metadata = Metadata {
            schema_version: "authorship/3.0.0".to_string(),
            git_ai_version: Some("development".to_string()),
            base_commit_sha: "base123".to_string(),
            prompts: BTreeMap::new(),
            sessions: BTreeMap::new(),
            humans: BTreeMap::new(),
        };

        let log = AuthorshipLog {
            attestations: vec![FileAttestation {
                file_path: "src/lib.rs".to_string(),
                entries: vec![AttestationEntry {
                    hash: "h_12345678901234".to_string(),
                    line_ranges: vec![LineRange::Range(1, 10)],
                }],
            }],
            metadata,
        };

        let mut commit_notes: HashMap<String, Option<AuthorshipLog>> = HashMap::new();
        commit_notes.insert("commit_xyz".to_string(), Some(log));

        let (author, prompt_hash) = resolve_line_author_with_prompt(
            "commit_xyz",
            5,
            "Alice",
            "alice@example.com",
            "src/lib.rs",
            &commit_notes,
            &[],
        );

        assert_eq!(author, "Alice");
        assert_eq!(prompt_hash, None);
    }

    #[test]
    fn resolve_line_author_with_prompt_session_hash() {
        let mut sessions = BTreeMap::new();
        sessions.insert(
            "s_12345678901234".to_string(),
            SessionRecord {
                agent_id: AgentId {
                    tool: "windsurf".to_string(),
                    id: "ws_session_1".to_string(),
                    model: "gpt-4".to_string(),
                },
                human_author: None,
                custom_attributes: None,
            },
        );

        let metadata = Metadata {
            schema_version: "authorship/3.0.0".to_string(),
            git_ai_version: Some("development".to_string()),
            base_commit_sha: "base123".to_string(),
            prompts: BTreeMap::new(),
            sessions,
            humans: BTreeMap::new(),
        };

        let log = AuthorshipLog {
            attestations: vec![FileAttestation {
                file_path: "src/app.rs".to_string(),
                entries: vec![AttestationEntry {
                    hash: "s_12345678901234".to_string(),
                    line_ranges: vec![LineRange::Range(10, 50)],
                }],
            }],
            metadata,
        };

        let mut commit_notes: HashMap<String, Option<AuthorshipLog>> = HashMap::new();
        commit_notes.insert("commit_sess".to_string(), Some(log));

        let (author, prompt_hash) = resolve_line_author_with_prompt(
            "commit_sess",
            25,
            "Bot User",
            "bot@example.com",
            "src/app.rs",
            &commit_notes,
            &[],
        );

        assert_eq!(author, "windsurf");
        assert_eq!(prompt_hash, Some("s_12345678901234".to_string()));
    }

    #[test]
    fn resolve_line_author_with_prompt_fallback_to_agent_email() {
        let commit_notes: HashMap<String, Option<AuthorshipLog>> = HashMap::new();

        let (author, prompt_hash) = resolve_line_author_with_prompt(
            "commit_no_note",
            1,
            "Claude",
            "noreply@anthropic.com",
            "src/file.rs",
            &commit_notes,
            &[],
        );

        assert_eq!(author, "claude");
        assert!(prompt_hash.is_some());
        // The prompt hash is a deterministic 16-char hex string
        assert_eq!(prompt_hash.as_ref().unwrap().len(), 16);
    }

    #[test]
    fn resolve_line_author_with_prompt_no_note_human_email() {
        let commit_notes: HashMap<String, Option<AuthorshipLog>> = HashMap::new();

        let (author, prompt_hash) = resolve_line_author_with_prompt(
            "commit_no_note",
            1,
            "Alice Developer",
            "alice@company.com",
            "src/file.rs",
            &commit_notes,
            &[],
        );

        assert_eq!(author, "Alice Developer");
        assert_eq!(prompt_hash, None);
    }

    #[test]
    fn resolve_line_author_with_prompt_note_present_but_no_matching_file() {
        let log = make_test_authorship_log_with_prompt();
        let mut commit_notes: HashMap<String, Option<AuthorshipLog>> = HashMap::new();
        commit_notes.insert("commit_abc".to_string(), Some(log));

        let (author, prompt_hash) = resolve_line_author_with_prompt(
            "commit_abc",
            5,
            "Human Author",
            "human@example.com",
            "src/other_file.rs", // not in the attestation
            &commit_notes,
            &[],
        );

        // Falls through to git_author since no agent email detected
        assert_eq!(author, "Human Author");
        assert_eq!(prompt_hash, None);
    }

    #[test]
    fn resolve_line_author_with_prompt_note_present_but_line_not_covered() {
        let log = make_test_authorship_log_with_prompt();
        let mut commit_notes: HashMap<String, Option<AuthorshipLog>> = HashMap::new();
        commit_notes.insert("commit_abc".to_string(), Some(log));

        let (author, prompt_hash) = resolve_line_author_with_prompt(
            "commit_abc",
            100, // line 100 is outside range 1-20
            "Human Author",
            "human@example.com",
            "src/main.rs",
            &commit_notes,
            &[],
        );

        assert_eq!(author, "Human Author");
        assert_eq!(prompt_hash, None);
    }
}
