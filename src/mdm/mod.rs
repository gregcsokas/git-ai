//! MDM (Managed Device Management) module — table-driven auto-installer for AI coding tool hooks.
//!
//! Detects which AI coding tools are installed, and writes hook configurations so they fire
//! `git-ai checkpoint <agent>` on every file edit.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Config table
// ---------------------------------------------------------------------------

/// How a tool's hooks config file is structured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookFormat {
    /// Cursor-style: `{ "hooks": { "preToolUse": [...], "postToolUse": [...] } }`
    CursorStyle,
    /// Same structure as Cursor but keys are PascalCase: PreToolUse / PostToolUse
    PascalCursorStyle,
    /// Claude Code style: `{ "hooks": { "PreToolUse": [{matcher, hooks: [...]}], ... } }`
    ClaudeStyle,
}

/// Static configuration for a single AI coding tool.
#[derive(Debug, Clone)]
struct AgentConfig {
    /// Tool/preset name (matches checkpoint agent name).
    name: &'static str,
    /// Config file path relative to $HOME. `None` means we can't auto-install yet.
    config_path: Option<&'static str>,
    /// Directory to check (relative to $HOME) to detect if the tool is installed.
    detect_dir: &'static str,
    /// Hook format variant.
    hook_format: Option<HookFormat>,
}

static AGENT_INSTALL_CONFIGS: &[AgentConfig] = &[
    AgentConfig {
        name: "cursor",
        config_path: Some(".cursor/hooks/hooks.json"),
        detect_dir: ".cursor",
        hook_format: Some(HookFormat::CursorStyle),
    },
    AgentConfig {
        name: "claude",
        config_path: Some(".claude/settings.json"),
        detect_dir: ".claude",
        hook_format: Some(HookFormat::ClaudeStyle),
    },
    AgentConfig {
        name: "windsurf",
        config_path: Some(".windsurf/hooks/hooks.json"),
        detect_dir: ".windsurf",
        hook_format: Some(HookFormat::CursorStyle),
    },
    AgentConfig {
        name: "amp",
        config_path: Some(".amp/hooks/hooks.json"),
        detect_dir: ".amp",
        hook_format: Some(HookFormat::PascalCursorStyle),
    },
    AgentConfig {
        name: "codex",
        config_path: Some(".codex/hooks/hooks.json"),
        detect_dir: ".codex",
        hook_format: Some(HookFormat::PascalCursorStyle),
    },
    AgentConfig {
        name: "gemini",
        config_path: None,
        detect_dir: ".gemini",
        hook_format: None,
    },
    AgentConfig {
        name: "pi",
        config_path: None,
        detect_dir: ".pi",
        hook_format: None,
    },
    AgentConfig {
        name: "opencode",
        config_path: None,
        detect_dir: ".opencode",
        hook_format: None,
    },
    AgentConfig {
        name: "droid",
        config_path: None,
        detect_dir: ".droid",
        hook_format: None,
    },
    AgentConfig {
        name: "github-copilot",
        config_path: None,
        detect_dir: ".github-copilot",
        hook_format: None,
    },
    AgentConfig {
        name: "firebender",
        config_path: None,
        detect_dir: ".firebender",
        hook_format: None,
    },
    AgentConfig {
        name: "continue-cli",
        config_path: None,
        detect_dir: ".continue",
        hook_format: None,
    },
];

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// An agent that was detected as installed on the system.
#[derive(Debug, Clone)]
pub struct InstalledAgent {
    pub name: String,
    pub config_path: Option<PathBuf>,
}

/// Status of a known agent.
#[derive(Debug, Clone)]
pub struct AgentStatus {
    pub name: String,
    /// Whether the tool's directory was detected on the system.
    pub detected: bool,
    /// Whether hooks are currently installed. `None` if detection dir missing or no config path.
    pub hooks_installed: Option<bool>,
    /// Whether auto-install is supported for this agent.
    pub installable: bool,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Returns which AI coding tools are detected on the system.
pub fn detect_installed() -> Vec<InstalledAgent> {
    let home = match home_dir() {
        Some(h) => h,
        None => return vec![],
    };

    AGENT_INSTALL_CONFIGS
        .iter()
        .filter(|cfg| home.join(cfg.detect_dir).is_dir())
        .map(|cfg| InstalledAgent {
            name: cfg.name.to_string(),
            config_path: cfg.config_path.map(|p| home.join(p)),
        })
        .collect()
}

/// Installs hooks for a specific tool by name.
pub fn install_hooks(tool: &str) -> Result<(), String> {
    let home = home_dir().ok_or_else(|| "could not determine HOME directory".to_string())?;
    let cfg = AGENT_INSTALL_CONFIGS
        .iter()
        .find(|c| c.name == tool)
        .ok_or_else(|| format!("unknown agent: {tool}"))?;

    let config_path = cfg
        .config_path
        .ok_or_else(|| format!("auto-install not supported for {tool}"))?;
    let hook_format = cfg
        .hook_format
        .ok_or_else(|| format!("hook format unknown for {tool}"))?;

    let full_path = home.join(config_path);
    let command = format!(
        "$HOME/.git-ai/bin/git-ai checkpoint {} --hook-input stdin",
        tool
    );

    install_hook_to_file(&full_path, &command, hook_format)
}

/// Installs hooks for all detected tools that support auto-install.
pub fn install_all() -> Vec<(String, Result<(), String>)> {
    let installed = detect_installed();
    installed
        .into_iter()
        .map(|agent| {
            let result = install_hooks(&agent.name);
            (agent.name, result)
        })
        .collect()
}

/// Returns install status for all known agents.
pub fn status() -> Vec<AgentStatus> {
    let home = match home_dir() {
        Some(h) => h,
        None => {
            return AGENT_INSTALL_CONFIGS
                .iter()
                .map(|cfg| AgentStatus {
                    name: cfg.name.to_string(),
                    detected: false,
                    hooks_installed: None,
                    installable: cfg.config_path.is_some(),
                })
                .collect();
        }
    };

    AGENT_INSTALL_CONFIGS
        .iter()
        .map(|cfg| {
            let detected = home.join(cfg.detect_dir).is_dir();
            let hooks_installed = if detected {
                cfg.config_path.and_then(|p| {
                    let full_path = home.join(p);
                    check_hooks_installed(&full_path, cfg.name)
                })
            } else {
                None
            };

            AgentStatus {
                name: cfg.name.to_string(),
                detected,
                hooks_installed,
                installable: cfg.config_path.is_some(),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::var("USERPROFILE").ok().map(PathBuf::from))
}

/// Check if git-ai hooks are already present in the config file.
fn check_hooks_installed(path: &Path, agent_name: &str) -> Option<bool> {
    let content = fs::read_to_string(path).ok()?;
    let needle = format!("git-ai checkpoint {}", agent_name);
    Some(content.contains(&needle))
}

/// Core install logic: read/parse/merge/write.
fn install_hook_to_file(path: &Path, command: &str, format: HookFormat) -> Result<(), String> {
    // Create parent directories if needed.
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create directories for {}: {}", path.display(), e))?;
    }

    // Read existing file or start with empty object.
    let mut root: Value = if path.exists() {
        let content = fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
        if content.trim().is_empty() {
            Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(&content)
                .map_err(|e| format!("failed to parse {}: {}", path.display(), e))?
        }
    } else {
        Value::Object(serde_json::Map::new())
    };

    // Check idempotency: if hook command already present, skip.
    let serialized = serde_json::to_string(&root).unwrap_or_default();
    if serialized.contains(command) {
        return Ok(());
    }

    // Merge hooks based on format.
    match format {
        HookFormat::CursorStyle => {
            merge_cursor_style(&mut root, command, "preToolUse", "postToolUse")?;
        }
        HookFormat::PascalCursorStyle => {
            merge_cursor_style(&mut root, command, "PreToolUse", "PostToolUse")?;
        }
        HookFormat::ClaudeStyle => {
            merge_claude_style(&mut root, command)?;
        }
    }

    // Write back pretty-printed.
    let output = serde_json::to_string_pretty(&root)
        .map_err(|e| format!("failed to serialize JSON: {}", e))?;
    fs::write(path, output.as_bytes())
        .map_err(|e| format!("failed to write {}: {}", path.display(), e))?;

    Ok(())
}

/// Merge Cursor-style hooks: `{ "hooks": { "<pre_key>": [...], "<post_key>": [...] } }`
fn merge_cursor_style(
    root: &mut Value,
    command: &str,
    pre_key: &str,
    post_key: &str,
) -> Result<(), String> {
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| "config root is not a JSON object".to_string())?;

    let hooks = root_obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));

    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| "\"hooks\" field is not a JSON object".to_string())?;

    let entry = serde_json::json!({"command": command});

    for key in [pre_key, post_key] {
        let arr = hooks_obj
            .entry(key)
            .or_insert_with(|| Value::Array(vec![]));
        let arr_vec = arr
            .as_array_mut()
            .ok_or_else(|| format!("\"hooks.{}\" is not an array", key))?;
        arr_vec.push(entry.clone());
    }

    Ok(())
}

/// Merge Claude Code style hooks.
fn merge_claude_style(root: &mut Value, command: &str) -> Result<(), String> {
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| "config root is not a JSON object".to_string())?;

    let hooks = root_obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));

    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| "\"hooks\" field is not a JSON object".to_string())?;

    let entry = serde_json::json!({
        "matcher": "*",
        "hooks": [{"type": "command", "command": command}]
    });

    for key in ["PreToolUse", "PostToolUse"] {
        let arr = hooks_obj
            .entry(key)
            .or_insert_with(|| Value::Array(vec![]));
        let arr_vec = arr
            .as_array_mut()
            .ok_or_else(|| format!("\"hooks.{}\" is not an array", key))?;
        arr_vec.push(entry.clone());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_command(agent: &str) -> String {
        format!("$HOME/.git-ai/bin/git-ai checkpoint {} --hook-input stdin", agent)
    }

    // --- Cursor-style tests ---

    #[test]
    fn test_cursor_style_empty_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");

        let cmd = make_command("cursor");
        install_hook_to_file(&path, &cmd, HookFormat::CursorStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let hooks = content["hooks"].as_object().unwrap();

        let pre = hooks["preToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0]["command"].as_str().unwrap(), cmd);

        let post = hooks["postToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 1);
        assert_eq!(post[0]["command"].as_str().unwrap(), cmd);
    }

    #[test]
    fn test_cursor_style_existing_settings() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");

        // Pre-existing config with other settings.
        let existing = serde_json::json!({
            "someOtherSetting": true,
            "hooks": {
                "preToolUse": [{"command": "echo existing"}]
            }
        });
        fs::write(&path, serde_json::to_string_pretty(&existing).unwrap()).unwrap();

        let cmd = make_command("cursor");
        install_hook_to_file(&path, &cmd, HookFormat::CursorStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();

        // Other settings preserved.
        assert_eq!(content["someOtherSetting"], Value::Bool(true));

        // Existing hook preserved, new hook added.
        let pre = content["hooks"]["preToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2);
        assert_eq!(pre[0]["command"].as_str().unwrap(), "echo existing");
        assert_eq!(pre[1]["command"].as_str().unwrap(), cmd);

        // Post hook created fresh.
        let post = content["hooks"]["postToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 1);
        assert_eq!(post[0]["command"].as_str().unwrap(), cmd);
    }

    #[test]
    fn test_cursor_style_idempotent() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");

        let cmd = make_command("cursor");
        install_hook_to_file(&path, &cmd, HookFormat::CursorStyle).unwrap();
        install_hook_to_file(&path, &cmd, HookFormat::CursorStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let pre = content["hooks"]["preToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1, "hook should not be duplicated");
    }

    // --- PascalCursor-style tests ---

    #[test]
    fn test_pascal_cursor_style_empty_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");

        let cmd = make_command("amp");
        install_hook_to_file(&path, &cmd, HookFormat::PascalCursorStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let hooks = content["hooks"].as_object().unwrap();

        assert!(hooks.contains_key("PreToolUse"));
        assert!(hooks.contains_key("PostToolUse"));
        assert_eq!(hooks["PreToolUse"].as_array().unwrap().len(), 1);
    }

    // --- Claude-style tests ---

    #[test]
    fn test_claude_style_empty_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");

        let cmd = make_command("claude");
        install_hook_to_file(&path, &cmd, HookFormat::ClaudeStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let hooks = content["hooks"].as_object().unwrap();

        let pre = hooks["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0]["matcher"].as_str().unwrap(), "*");
        let inner = pre[0]["hooks"].as_array().unwrap();
        assert_eq!(inner[0]["type"].as_str().unwrap(), "command");
        assert_eq!(inner[0]["command"].as_str().unwrap(), cmd);
    }

    #[test]
    fn test_claude_style_existing_settings() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");

        let existing = serde_json::json!({
            "permissions": {"allow": ["bash"]},
            "hooks": {
                "PreToolUse": [{"matcher": "write", "hooks": [{"type": "command", "command": "echo lint"}]}]
            }
        });
        fs::write(&path, serde_json::to_string_pretty(&existing).unwrap()).unwrap();

        let cmd = make_command("claude");
        install_hook_to_file(&path, &cmd, HookFormat::ClaudeStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();

        // Permissions preserved.
        assert!(content["permissions"]["allow"].as_array().unwrap().len() == 1);

        // Existing hook preserved, new hook appended.
        let pre = content["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2);
        assert_eq!(pre[0]["matcher"].as_str().unwrap(), "write");
        assert_eq!(pre[1]["matcher"].as_str().unwrap(), "*");
    }

    #[test]
    fn test_claude_style_idempotent() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");

        let cmd = make_command("claude");
        install_hook_to_file(&path, &cmd, HookFormat::ClaudeStyle).unwrap();
        install_hook_to_file(&path, &cmd, HookFormat::ClaudeStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let pre = content["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1, "hook should not be duplicated");
    }

    // --- detect/status tests ---

    #[test]
    fn test_detect_with_fake_home() {
        let tmp = TempDir::new().unwrap();
        // Create .cursor dir to simulate cursor installed.
        fs::create_dir(tmp.path().join(".cursor")).unwrap();

        // Temporarily override HOME.
        // SAFETY: test is single-threaded and we accept the risk of env mutation.
        unsafe { std::env::set_var("HOME", tmp.path()) };
        let detected = detect_installed();

        let names: Vec<&str> = detected.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"cursor"));
        assert!(!names.contains(&"claude"));
    }

    #[test]
    fn test_status_with_fake_home() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".cursor")).unwrap();

        // SAFETY: test is single-threaded and we accept the risk of env mutation.
        unsafe { std::env::set_var("HOME", tmp.path()) };
        let statuses = status();

        let cursor_status = statuses.iter().find(|s| s.name == "cursor").unwrap();
        assert!(cursor_status.detected);
        assert!(cursor_status.installable);
        // No config file exists yet, so hooks_installed is None (file doesn't exist).
        assert_eq!(cursor_status.hooks_installed, None);

        let gemini_status = statuses.iter().find(|s| s.name == "gemini").unwrap();
        assert!(!gemini_status.detected);
        assert!(!gemini_status.installable);
    }

    #[test]
    fn test_install_hooks_creates_directories() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".cursor")).unwrap();
        // SAFETY: test is single-threaded and we accept the risk of env mutation.
        unsafe { std::env::set_var("HOME", tmp.path()) };

        let result = install_hooks("cursor");
        assert!(result.is_ok(), "install_hooks failed: {:?}", result);

        let hook_path = tmp.path().join(".cursor/hooks/hooks.json");
        assert!(hook_path.exists());

        let content: Value =
            serde_json::from_str(&fs::read_to_string(&hook_path).unwrap()).unwrap();
        assert!(content["hooks"]["preToolUse"].as_array().unwrap().len() == 1);
    }

    #[test]
    fn test_install_unknown_agent() {
        let result = install_hooks("nonexistent-tool");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown agent"));
    }

    #[test]
    fn test_install_unsupported_agent() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".gemini")).unwrap();
        // SAFETY: test is single-threaded and we accept the risk of env mutation.
        unsafe { std::env::set_var("HOME", tmp.path()) };

        let result = install_hooks("gemini");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not supported"));
    }

    #[test]
    fn test_install_all() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".cursor")).unwrap();
        fs::create_dir(tmp.path().join(".claude")).unwrap();
        // SAFETY: test is single-threaded and we accept the risk of env mutation.
        unsafe { std::env::set_var("HOME", tmp.path()) };

        let results = install_all();
        let names: Vec<&str> = results.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"cursor"));
        assert!(names.contains(&"claude"));

        for (name, result) in &results {
            assert!(result.is_ok(), "{} install failed: {:?}", name, result);
        }
    }
}
