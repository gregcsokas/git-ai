use crate::commands::helpers::git_cmd;

pub fn handle_status(args: &[String]) {
    if args.iter().any(|a| a == "--json") {
        println!("{{}}");
    } else {
        println!("No uncommitted attributions.");
    }
}

pub fn handle_stats(args: &[String]) {
    let is_json = args.iter().any(|a| a == "--json");
    let commit_ref = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map(|s| s.as_str())
        .unwrap_or("HEAD");

    let commit_sha = match git_cmd(&["rev-parse", commit_ref]) {
        Ok(s) => s,
        Err(_) => {
            if is_json {
                println!("{{}}");
            } else {
                println!("No stats available.");
            }
            return;
        }
    };

    let note = match git_cmd(&["notes", "--ref=ai", "show", &commit_sha]) {
        Ok(n) => n,
        Err(_) => {
            if is_json {
                println!("{{}}");
            } else {
                println!("No stats available.");
            }
            return;
        }
    };

    let log = match git_ai::core::authorship_log::AuthorshipLog::deserialize_from_string(&note) {
        Ok(l) => l,
        Err(_) => {
            if is_json {
                println!("{{}}");
            } else {
                println!("No stats available.");
            }
            return;
        }
    };

    let mut ai_additions: u64 = 0;
    let mut human_additions: u64 = 0;

    for file_att in &log.attestations {
        for entry in &file_att.entries {
            let count: u64 = entry
                .line_ranges
                .iter()
                .map(|r| r.line_count() as u64)
                .sum();
            if entry.hash.starts_with("h_") {
                human_additions += count;
            } else {
                ai_additions += count;
            }
        }
    }

    if is_json {
        println!(
            "{{\"ai_additions\":{},\"human_additions\":{},\"files\":{{\"total\":{{}}}}}}",
            ai_additions, human_additions
        );
    } else {
        println!("AI additions: {}", ai_additions);
        println!("Human additions: {}", human_additions);
    }
}
