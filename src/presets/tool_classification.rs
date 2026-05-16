//! Tool classification: maps (agent, tool_name) → FileEdit | Bash | Skip.
//!
//! This is the single source of truth for which tools produce file changes
//! vs shell commands vs should be ignored.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolClass {
    FileEdit,
    Bash,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Gemini,
    ContinueCli,
    Droid,
    Amp,
    OpenCode,
    Firebender,
    Codex,
    Pi,
    Windsurf,
    Cursor,
    GithubCopilot,
    AiTab,
}

pub fn classify_tool(agent: Agent, tool_name: &str) -> ToolClass {
    match agent {
        Agent::Claude => match tool_name {
            "Write" | "Edit" | "MultiEdit" => ToolClass::FileEdit,
            "Bash" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::Gemini => match tool_name {
            "write_file" | "replace" => ToolClass::FileEdit,
            "shell" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::ContinueCli => match tool_name {
            "edit" => ToolClass::FileEdit,
            "terminal" | "local_shell_call" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::Droid => match tool_name {
            "ApplyPatch" | "Edit" | "Write" | "Create" => ToolClass::FileEdit,
            "Bash" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::Amp => match tool_name {
            "Write" | "Edit" => ToolClass::FileEdit,
            "Bash" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::OpenCode => match tool_name {
            "edit" | "write" => ToolClass::FileEdit,
            "bash" | "shell" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::Firebender => match tool_name {
            "Write" | "Edit" | "Delete" | "RenameSymbol" | "DeleteSymbol" => ToolClass::FileEdit,
            "Bash" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::Codex => match tool_name {
            "apply_patch" => ToolClass::FileEdit,
            "Bash" | "exec_command" | "shell" | "shell_command" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::Pi => match tool_name {
            "edit" | "write" | "replace" | "rename" => ToolClass::FileEdit,
            "bash" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::Windsurf => match tool_name {
            "code_action" => ToolClass::FileEdit,
            "run_command" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::Cursor => match tool_name {
            "Write" | "Delete" | "StrReplace" => ToolClass::FileEdit,
            "Shell" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::GithubCopilot => match tool_name {
            "copilot_replaceString"
            | "create_file"
            | "apply_patch"
            | "editFiles"
            | "insert_edit"
            | "replace_edit"
            | "delete_edit"
            | "replace_string_in_file"
            | "replaceStringInFile"
            | "edit"
            | "create" => ToolClass::FileEdit,
            "runInTerminal" | "run_in_terminal" => ToolClass::Bash,
            // GitHub Copilot's before_edit/after_edit events may not include a tool_name;
            // default to FileEdit when tool_name is empty (the event type itself implies file edit).
            "" => ToolClass::FileEdit,
            _ => ToolClass::Skip,
        },
        Agent::AiTab => ToolClass::FileEdit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_tools() {
        assert_eq!(classify_tool(Agent::Cursor, "Write"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Cursor, "Delete"), ToolClass::FileEdit);
        assert_eq!(
            classify_tool(Agent::Cursor, "StrReplace"),
            ToolClass::FileEdit
        );
        assert_eq!(classify_tool(Agent::Cursor, "Shell"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::Cursor, "Read"), ToolClass::Skip);
        assert_eq!(classify_tool(Agent::Cursor, "unknown"), ToolClass::Skip);
    }

    #[test]
    fn claude_tools() {
        assert_eq!(classify_tool(Agent::Claude, "Write"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Claude, "Edit"), ToolClass::FileEdit);
        assert_eq!(
            classify_tool(Agent::Claude, "MultiEdit"),
            ToolClass::FileEdit
        );
        assert_eq!(classify_tool(Agent::Claude, "Bash"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::Claude, "Read"), ToolClass::Skip);
    }

    #[test]
    fn gemini_tools() {
        assert_eq!(
            classify_tool(Agent::Gemini, "write_file"),
            ToolClass::FileEdit
        );
        assert_eq!(classify_tool(Agent::Gemini, "replace"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Gemini, "shell"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::Gemini, "read_file"), ToolClass::Skip);
    }

    #[test]
    fn codex_tools() {
        assert_eq!(
            classify_tool(Agent::Codex, "apply_patch"),
            ToolClass::FileEdit
        );
        assert_eq!(classify_tool(Agent::Codex, "Bash"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::Codex, "exec_command"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::Codex, "shell"), ToolClass::Bash);
        assert_eq!(
            classify_tool(Agent::Codex, "shell_command"),
            ToolClass::Bash
        );
    }

    #[test]
    fn copilot_tools() {
        assert_eq!(
            classify_tool(Agent::GithubCopilot, "copilot_replaceString"),
            ToolClass::FileEdit
        );
        assert_eq!(
            classify_tool(Agent::GithubCopilot, "create_file"),
            ToolClass::FileEdit
        );
        assert_eq!(
            classify_tool(Agent::GithubCopilot, "runInTerminal"),
            ToolClass::Bash
        );
        assert_eq!(
            classify_tool(Agent::GithubCopilot, "unknown"),
            ToolClass::Skip
        );
    }

    #[test]
    fn windsurf_tools() {
        assert_eq!(
            classify_tool(Agent::Windsurf, "code_action"),
            ToolClass::FileEdit
        );
        assert_eq!(
            classify_tool(Agent::Windsurf, "run_command"),
            ToolClass::Bash
        );
        assert_eq!(classify_tool(Agent::Windsurf, "search"), ToolClass::Skip);
    }

    #[test]
    fn amp_tools() {
        assert_eq!(classify_tool(Agent::Amp, "Write"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Amp, "Edit"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Amp, "Bash"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::Amp, "Read"), ToolClass::Skip);
    }

    #[test]
    fn unknown_tool_names_return_skip_for_all_agents() {
        let agents = [
            Agent::Claude,
            Agent::Gemini,
            Agent::ContinueCli,
            Agent::Droid,
            Agent::Amp,
            Agent::OpenCode,
            Agent::Firebender,
            Agent::Codex,
            Agent::Pi,
            Agent::Windsurf,
            Agent::Cursor,
            Agent::GithubCopilot,
        ];
        for agent in agents {
            assert_eq!(
                classify_tool(agent, "completely_unknown_tool_xyz"),
                ToolClass::Skip,
                "Agent {:?} should return Skip for unknown tool",
                agent
            );
        }
    }

    #[test]
    fn aitab_always_returns_file_edit() {
        // AiTab returns FileEdit regardless of tool name
        assert_eq!(classify_tool(Agent::AiTab, "anything"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::AiTab, ""), ToolClass::FileEdit);
        assert_eq!(
            classify_tool(Agent::AiTab, "unknown_tool"),
            ToolClass::FileEdit
        );
        assert_eq!(classify_tool(Agent::AiTab, "Bash"), ToolClass::FileEdit);
    }

    #[test]
    fn tool_names_are_case_sensitive() {
        // Claude tools are PascalCase - lowercase should skip
        assert_eq!(classify_tool(Agent::Claude, "write"), ToolClass::Skip);
        assert_eq!(classify_tool(Agent::Claude, "edit"), ToolClass::Skip);
        assert_eq!(classify_tool(Agent::Claude, "bash"), ToolClass::Skip);
        assert_eq!(classify_tool(Agent::Claude, "WRITE"), ToolClass::Skip);

        // Gemini tools are snake_case - PascalCase should skip
        assert_eq!(classify_tool(Agent::Gemini, "Write_file"), ToolClass::Skip);
        assert_eq!(classify_tool(Agent::Gemini, "Shell"), ToolClass::Skip);

        // Cursor's Shell is PascalCase
        assert_eq!(classify_tool(Agent::Cursor, "shell"), ToolClass::Skip);
        assert_eq!(classify_tool(Agent::Cursor, "SHELL"), ToolClass::Skip);
    }

    #[test]
    fn continue_cli_tools() {
        assert_eq!(
            classify_tool(Agent::ContinueCli, "edit"),
            ToolClass::FileEdit
        );
        assert_eq!(
            classify_tool(Agent::ContinueCli, "terminal"),
            ToolClass::Bash
        );
        assert_eq!(
            classify_tool(Agent::ContinueCli, "local_shell_call"),
            ToolClass::Bash
        );
        assert_eq!(classify_tool(Agent::ContinueCli, "read"), ToolClass::Skip);
    }

    #[test]
    fn droid_tools() {
        assert_eq!(
            classify_tool(Agent::Droid, "ApplyPatch"),
            ToolClass::FileEdit
        );
        assert_eq!(classify_tool(Agent::Droid, "Edit"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Droid, "Write"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Droid, "Create"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Droid, "Bash"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::Droid, "Read"), ToolClass::Skip);
    }

    #[test]
    fn opencode_tools() {
        assert_eq!(classify_tool(Agent::OpenCode, "edit"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::OpenCode, "write"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::OpenCode, "bash"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::OpenCode, "shell"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::OpenCode, "read"), ToolClass::Skip);
    }

    #[test]
    fn firebender_tools() {
        assert_eq!(
            classify_tool(Agent::Firebender, "Write"),
            ToolClass::FileEdit
        );
        assert_eq!(
            classify_tool(Agent::Firebender, "Edit"),
            ToolClass::FileEdit
        );
        assert_eq!(
            classify_tool(Agent::Firebender, "Delete"),
            ToolClass::FileEdit
        );
        assert_eq!(
            classify_tool(Agent::Firebender, "RenameSymbol"),
            ToolClass::FileEdit
        );
        assert_eq!(
            classify_tool(Agent::Firebender, "DeleteSymbol"),
            ToolClass::FileEdit
        );
        assert_eq!(classify_tool(Agent::Firebender, "Bash"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::Firebender, "Read"), ToolClass::Skip);
    }

    #[test]
    fn pi_tools() {
        assert_eq!(classify_tool(Agent::Pi, "edit"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Pi, "write"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Pi, "replace"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Pi, "rename"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Pi, "bash"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::Pi, "read"), ToolClass::Skip);
    }

    #[test]
    fn codex_shell_variants() {
        // All shell-like tool variants for Codex
        assert_eq!(classify_tool(Agent::Codex, "Bash"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::Codex, "exec_command"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::Codex, "shell"), ToolClass::Bash);
        assert_eq!(
            classify_tool(Agent::Codex, "shell_command"),
            ToolClass::Bash
        );
        // File edit
        assert_eq!(
            classify_tool(Agent::Codex, "apply_patch"),
            ToolClass::FileEdit
        );
        // Others skip
        assert_eq!(classify_tool(Agent::Codex, "read_file"), ToolClass::Skip);
        assert_eq!(classify_tool(Agent::Codex, "search"), ToolClass::Skip);
    }

    #[test]
    fn copilot_empty_tool_name_is_file_edit() {
        // Empty tool_name for GithubCopilot implies file edit (before_edit/after_edit events)
        assert_eq!(classify_tool(Agent::GithubCopilot, ""), ToolClass::FileEdit);
    }

    #[test]
    fn copilot_all_file_edit_variants() {
        let file_edit_tools = [
            "copilot_replaceString",
            "create_file",
            "apply_patch",
            "editFiles",
            "insert_edit",
            "replace_edit",
            "delete_edit",
            "replace_string_in_file",
            "replaceStringInFile",
            "edit",
            "create",
        ];
        for tool in file_edit_tools {
            assert_eq!(
                classify_tool(Agent::GithubCopilot, tool),
                ToolClass::FileEdit,
                "GithubCopilot tool '{}' should be FileEdit",
                tool
            );
        }
    }

    #[test]
    fn copilot_terminal_variants() {
        assert_eq!(
            classify_tool(Agent::GithubCopilot, "runInTerminal"),
            ToolClass::Bash
        );
        assert_eq!(
            classify_tool(Agent::GithubCopilot, "run_in_terminal"),
            ToolClass::Bash
        );
    }
}
