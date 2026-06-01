# git-ai   <a href="https://discord.gg/XJStYvkb5U"><img alt="Discord" src="https://img.shields.io/badge/discord-join-5865F2?logo=discord&logoColor=white" /></a>        

<img src="https://github.com/git-ai-project/git-ai/raw/main/assets/docs/git-ai.png" align="right"
     alt="Git AI Logo" width="200" height="200">

Git AI is an open source git extension that tracks AI-generated code in your repositories.

Once installed, it automatically links every AI-written line to the agent, model, and transcript that generated it — so you never lose the intent, requirements, and architecture decisions behind your code.

**AI attribution on every commit:**

```
[hooks-doctor 0afe44b2] wsl compat check
2 files changed, 81 insertions(+), 3 deletions(-)

you  ██░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░ ai
     6%             mixed   2%             92%
```

**AI Blame shows the model, agent, and session behind every line:**

`git ai blame /src/log_fmt/authorship_log.rs`
```bash

cb832b7 (Aidan Cunniffe      2025-12-13 08:16:29 -0500  133) pub fn execute_diff(
cb832b7 (Aidan Cunniffe      2025-12-13 08:16:29 -0500  134)     repo: &Repository,
cb832b7 (Aidan Cunniffe      2025-12-13 08:16:29 -0500  135)     spec: DiffSpec,
cb832b7 (Aidan Cunniffe      2025-12-13 08:16:29 -0500  136)     format: DiffFormat,
cb832b7 (Aidan Cunniffe      2025-12-13 08:16:29 -0500  137) ) -> Result<String, GitAiError> {
fe2c4c8 (claude              2025-12-02 19:25:13 -0500  138)     // Resolve commits to get from/to SHAs
fe2c4c8 (claude              2025-12-02 19:25:13 -0500  139)     let (from_commit, to_commit) = match spec {
fe2c4c8 (claude              2025-12-02 19:25:13 -0500  140)         DiffSpec::TwoCommit(start, end) => {
fe2c4c8 (claude              2025-12-02 19:25:13 -0500  141)             // Resolve both commits
fe2c4c8 (claude              2025-12-02 19:25:13 -0500  142)             let from = resolve_commit(repo, &start)?;...
```

**See your personal AI-usage**

..usage command goes here...

## Install

**Mac, Linux, Windows (WSL)**

```bash
curl -sSL https://usegitai.com/install.sh | bash
```

**Windows**

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://usegitai.com/install.ps1 | iex"
```

That's it — **no per-repo setup or git hooks required.** Commit with the Agent, the CLI or your Git AI. Attribution is captured and linked to commits automatically.

**The Git AI standard is supported by:**
<table>
<tr>
<td align="center" width="20%"><img src="assets/docs/agents/gray/claude_code.png" alt="Claude Code" width="160" /></td>
<td align="center" width="20%"><img src="assets/docs/agents/gray/codex-black.png" alt="Codex" width="160" /></td>
<td align="center" width="20%"><img src="assets/docs/agents/gray/cursor.png" alt="Cursor" width="160" /></td>
<td align="center" width="20%"><img src="assets/docs/agents/gray/copilot.png" alt="GitHub Copilot" width="160" /></td>
<td align="center" width="20%"><img src="assets/docs/agents/gray/opencode.png" alt="OpenCode" width="160" /></td>
</tr>
<tr>
<td align="center"><img src="assets/docs/agents/gray/pi.png" alt="Pi" width="160" /></td>
<td align="center"><img src="assets/docs/agents/gray/windsurf.png" alt="Windsurf" width="160" /></td>
<td align="center"><img src="assets/docs/agents/gray/droid.png" alt="Droid" width="160" /></td>
<td align="center"><img src="assets/docs/agents/gray/amp.png" alt="Amp" width="160" /></td>
<td align="center"><img src="assets/docs/agents/gray/gemini.png" alt="Gemini" width="160" /></td>
</tr>
<tr>
<td align="center"><img src="assets/docs/agents/gray/continue.png" alt="Continue" width="160" /></td>
<td align="center"><img src="assets/docs/agents/gray/junie_white.png" alt="Junie" width="160" /></td>
<td align="center"><img src="assets/docs/agents/gray/rovodev.png" alt="Rovo Dev" width="160" /></td>
<td align="center"><img src="assets/docs/agents/gray/firebender.png" alt="Firebender" width="160" /></td>
<td align="center"><a href="https://usegitai.com/docs/cli/add-your-agent">+ Add an Agent</a></td>
</tr>
</table>

## Use with your Team 

<table>
<tr>
<td width="50%" valign="top">

### Open source CI Workflows

<a href="https://calendly.com/d/cxjh-z79-ktm/meeting-with-git-ai-authors" target="_blank"><img src="assets/docs/buttons/meet-the-maintainers.svg" alt="Meet the maintainers" height="35" /></a>

Persistent line level AI-attribution for every repository:

- Measure **% AI** per commit, PR, and contributor
- **Line-level attribution** on every commit
- **Model and agent tracking** — know exactly which agent and model wrote each line, including accepted rate

To get started, ask your teammates to install Git AI and add the CI Actions. 

</td>
<td width="50%" valign="top">

Add the [**Open Source CI Actions**](https://usegitai.com/docs/guides/ci-workflows) to your repos so attributions are preserved when you **Squash Merge** and **Rebase Merge**. Process the data however you like — pipe it into your own dashboards with the [`git ai stats`](https://usegitai.com/docs/cli) command ([CLI reference →](https://usegitai.com/docs/cli))


</td>
</tr>
<tr>
<td width="50%" valign="top">

### For Teams and Enterprises

<a href="https://usegitai.com/book-demo" target="_blank"><img src="assets/docs/buttons/get-early-access.svg" alt="Get early access" height="35" /></a>


**Observability for your Software Factory.** Connect your SCM once and get aggregate data across thousands of repos plus full observability into everything your coding agents do:

- See how much AI-code makes it all the way to production
- Measure **% AI** and token spend by Pull Request, Repo, Team, and Contributor
- Tie incidents back to AI-sessions
- Measure and improve Agent autonomy and token efficiency
- Compute AI-code durability and how much rework is required during Code Review and immediately after shipping
- Measure the ROI of AI spend
- Save prompts behind every generated hunk of code and share summaries with teammates
- Run on Git AI's Cloud or self-host 


To get started, [setup a call](https://usegitai.com/book-demo) with the maintainers. 

</td>
<td width="50%" valign="top">

<a href="https://github.com/user-attachments/assets/416d9597-18eb-4243-b38f-ace8cb684cac"><img src="assets/docs/dashboard.png" alt="Git AI for Teams dashboard — watch the demo" width="100%" /></a>

<sub><i>▶ Watch the 2-minute demo</i></sub>

Our team will help you get set up the platform, install Git AI on every developer's machine, put the data to work.  

</td>
</tr>
</table>


### How it works:  

1. Coding Agents that support Git AI's standard call `git-ai checkpoint` whenever they write code or modify files with bash scripts. 
1. Git AI stores this attribution data in Git Notes, linking each line of AI-generated code to the agent, model, and session that created it. Run `git log --show-notes="ai"` to see them. 
1. Git AI moves and merges line-level attributions when you `squash`, `merge`, `reset`, `rebase`, `stash`, `cherry-pick`, etc. so your AI code is always accurately tracked.

*Git AI does not "detect" AI code — the Agents report exactly which lines they wrote, providing the most accurate, explicit attribution possible.*

### Our Choices

- **Transparent** — Git AI requires no workflow changes. Just prompt and commit as you normally would and Git AI automatically attaches attribution metadata to every commit. 
- **No performance overhead** — Git AI does not rely on Git Hooks (slow, hard to set up in every repo) and it does not wrap the Git binary. It runs outside the hotpath so your Git operations are just as fast as they would be without Git AI. 
- **Local-first** — Works offline, no login required.
- **Secure Prompt Storage** — Git AI links each line of AI-code back to the prompt that generated it. These sessions scanned and redacted, and saved outside of Git -- keeping repos lean, enabling fine-grained access control, and preventing PII or secrets from leaking. Learn more about setting up a prompt store here. 
- **Git native and open standard** — Git AI built the [open standard](https://github.com/git-ai-project/git-ai/blob/main/specs/git_ai_standard_v3.0.0.md) for tracking AI-generated code with Git Notes.

### FAQ

**Does the agent have to commit for Git AI to attribute the code?**
No. Git AI works no matter how you commit — your Git client, the Git CLI, and your own Git aliases are all supported.

**Git AI notes are attached to commits — how are attributions preserved when I rebase, squash, stash, cherry-pick, etc.?**
Git AI analyzes the final state of the code after the operation completes and copies/merges the attributions into a Git Note for any completed commits. It's eventually consistent. The note will be written 5-100ms after the operation completes.

**Can I use this on my own?**
Yes. Git AI is free and open source, works locally, and requires no login or team setup.

**Is there a performance impact?**
No. Git AI does not use Git hooks and it does not wrap Git, so you won't see any overhead on your Git commands.

**Do I have to set up agent hooks?**
Nope — Git AI manages the agent hooks and checks/updates them daily. If you want to trigger this yourself (ie just installed a new agent) run `git ai install-hooks`.

**Who uses this?**
Hundreds of engineering teams, several in the Fortune 100, use Git AI to understand their AI usage and make agents more effective on their codebase.

**What's the difference between the open source CLI and the [teams version](https://usegitai.com)?**
The CLI accurately attributes AI code on every commit. The teams version adds a secure prompt store and joins in data from across the SDLC — tying token spend to individual Pull Requests, calculating % AI by PR, team, and repo, and connecting signals like amount of rework during code review, and even tying incidents back to the AI session that caused them. Self-host it or run it in our cloud: connect your SCM and get aggregate stats across thousands of repos plus full observability into everything your coding agents do. [Meet the maintainers](https://calendly.com/d/cxjh-z79-ktm/meeting-with-git-ai-authors) for a demo and early access.

**Who built this?**
Aidan and Sasha — say hi in [Discord](https://discord.gg/XJStYvkb5U) or set up a [Meet the maintainers call](https://calendly.com/d/cxjh-z79-ktm/meeting-with-git-ai-authors).

**What are the capabilities and known limitations?**
Git AI provides line-level attribution for AI-generated code - whether it is written with an edit tool or a bash command. When a  Git rewrite operation is run (`rebase`, `stash`, `squash --merge`, etc) Git AI will move and merge attributions so nothing is lost. 

Here is a full breakdown of what is supported today: 

| Capability                                                      | Status | Notes                                                                        |
| --------------------------------------------------------------- | ------ | ---------------------------------------------------------------------------- |
| Edit / Write / Patch tools                                      | ✅      | Line-level attribution recorded automatically.                               |
| Files created via Bash                                          | ✅      | May not work if the agent is not operating from the repository root.         |
| Git worktrees                                                   | ✅      | Attribution maintained across linked worktrees.                              |
| Background Agents                                               | ✅      | See docs for [Claude Web](https://usegitai.com/docs/cli/claude-web), [Codex Cloud](https://usegitai.com/docs/cli/codex-cloud), [Cursor Agent](https://usegitai.com/docs/cli/cursor-agent), and [Devin](https://usegitai.com/docs/cli/devin). |
| Attribute lines from multiple Agent Sessions in the same commit | ✅      |                                                                              |
| Record which lines a human overrode                             | ✅      |                                                                              |
| Attribute sessions that produced no code                        | ✅      | Records token usage and session activity even when no code is accepted.      |
| Accepted rate per session                                       | ✅      |                                                                              |
| Added and deleted lines per session                             | ✅      |                                                                              |
| Tool-call level attribution                                     | ✅      | Resolves attributed lines to the tool call that generated them.              |
| Tokens and cost per commit and PR                               | ✅      | Aggregates token usage and cost across the sessions behind each commit/PR.   |
| Formatters                                                      | ✅      | Formatting will not change attribution to human.                             |
| Multi-repo root                                                 | ⚠️     | If you run an agent that edits multiple repos, Bash attributions only work when the agent runs each command with its cwd inside that repo. |

Git Rewrite Operations:

| Operation                                                       | Status | Notes                                                                        |
| --------------------------------------------------------------- | ------ | ---------------------------------------------------------------------------- |
| `git rebase`                                                    | ✅      | Attribution preserved. [View Code](https://github.com/git-ai-project/git-ai/blob/f3da782e93c492303e44d14805179123d1740e7f/src/daemon.rs#L6578-L6664) |
| `git cherry-pick`                                               | ✅      | Attribution preserved. [View Code](https://github.com/git-ai-project/git-ai/blob/f3da782e93c492303e44d14805179123d1740e7f/src/daemon.rs#L6675-L6718) |
| `git stash` / `git stash pop`                                  | ✅      | Attribution preserved. [View Code](https://github.com/git-ai-project/git-ai/blob/f3da782e93c492303e44d14805179123d1740e7f/src/daemon.rs#L6758-L6824) |
| `git merge --squash`                                            | ✅      | Attribution preserved. [View Code](https://github.com/git-ai-project/git-ai/blob/f3da782e93c492303e44d14805179123d1740e7f/src/daemon.rs#L6729-L6757) |
| `git reset --soft`                                              | ✅      | Attribution preserved. [View Code](https://github.com/git-ai-project/git-ai/blob/f3da782e93c492303e44d14805179123d1740e7f/src/daemon.rs#L6504-L6577) |
| `git reset --mixed`                                            | ✅      | Attribution preserved. [View Code](https://github.com/git-ai-project/git-ai/blob/f3da782e93c492303e44d14805179123d1740e7f/src/daemon.rs#L6504-L6577) |
| `git reset --hard`                                              | ✅      | Attribution preserved for commits that remain in history. [View Code](https://github.com/git-ai-project/git-ai/blob/f3da782e93c492303e44d14805179123d1740e7f/src/daemon.rs#L6504-L6577) |
| `git merge` (merge commit)                                      | ✅      | Attribution preserved. [View Code](https://github.com/git-ai-project/git-ai/blob/f3da782e93c492303e44d14805179123d1740e7f/src/daemon.rs#L6475-L6485) |
| `git commit --amend`                                            | ✅      | Attribution preserved, including unstaged and partially staged changes. [View Code](https://github.com/git-ai-project/git-ai/blob/f3da782e93c492303e44d14805179123d1740e7f/src/daemon.rs#L6486-L6503) |
| `git checkout` / `git switch` (branches)                       | ✅      | Attribution follows the working tree across branch changes. [View Code](https://github.com/git-ai-project/git-ai/blob/f3da782e93c492303e44d14805179123d1740e7f/src/daemon.rs#L966-L978) |
| `git pull` (fast-forward / `--rebase`)                          | ✅      | Attribution preserved, including autostashed changes. [View Code](https://github.com/git-ai-project/git-ai/blob/f3da782e93c492303e44d14805179123d1740e7f/src/daemon.rs#L6825-L6874) |
| `git push` / `git fetch`                                       | ✅      | Attribution notes synced to/from the remote. [View Code](https://github.com/git-ai-project/git-ai/blob/f3da782e93c492303e44d14805179123d1740e7f/src/commands/hooks/push_hooks.rs#L7-L30) |
| `git mv`                                                        | ❌      | Renames are not yet tracked; attribution does not follow the moved file.     |
| `git filter-branch` / `git filter-repo`                        | ❌      | Bulk history rewrites are not tracked.                                        |
| `git replace`                                                  | ❌      | Object replacements are not tracked.                                         |


GitHub, GitLab, BitBucket, Azure DevOps:

| Capability                                                      | Status | Notes                                                                        |
| --------------------------------------------------------------- | ------ | ---------------------------------------------------------------------------- |
| Squash and Merge                                                | ✅      | Requires [Git AI for Teams](https://usegitai.com/book-demo) or [Open Source CI Actions](https://usegitai.com/docs/guides/ci-workflows) to preserve attribution. |
| Rebase and Merge                                                | ✅      | Requires [Git AI for Teams](https://usegitai.com/book-demo) or [Open Source CI Actions](https://usegitai.com/docs/guides/ci-workflows) to preserve attribution. |



## License
Apache 2.0
