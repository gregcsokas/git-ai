# Daemon Plan (v2)

The daemon is the brain of git-ai v2. It listens for git events via trace2,
processes checkpoints from AI coding agents, and generates authorship notes
on commits — all without requiring a git wrapper/proxy.

## Architecture Overview

```
┌──────────────────────────────────────────────────────┐
│                    git-ai daemon                      │
│                                                      │
│  ┌─────────────┐   ┌──────────────┐   ┌──────────┐  │
│  │ Trace2      │   │ Control      │   │ Core     │  │
│  │ Listener    │──▶│ Coordinator  │──▶│ Engine   │  │
│  │ (AF_UNIX)   │   │              │   │          │  │
│  └─────────────┘   └──────────────┘   └──────────┘  │
│                          ▲                           │
│  ┌─────────────┐         │                           │
│  │ Control     │─────────┘                           │
│  │ Socket      │                                     │
│  │ (AF_UNIX)   │                                     │
│  └─────────────┘                                     │
└──────────────────────────────────────────────────────┘
         ▲                          ▲
         │ trace2 events            │ checkpoint requests
         │ (from git)               │ (from agents)
```

**Key principle:** The daemon does NOT replace `git-ai checkpoint` or
`git-ai post-commit` — those remain usable as standalone CLI commands.
The daemon adds *automatic* post-commit detection so users don't need
to install git hooks.

---

## Implementation Phases

### Phase 1: Minimal daemon loop (detect commits, generate notes)

The smallest useful daemon: listen on a trace2 socket, detect when
`git commit` completes, and run authorship generation.

- [ ] **1.1 Daemon skeleton** — `src/daemon/mod.rs`
  - Process lifecycle: fork/daemonize, PID file, lock file
  - Signal handling (SIGTERM, SIGINT → graceful shutdown)
  - Logging to `~/.git-ai/internal/daemon/daemon.log`
  - `git-ai bg run` command to start in foreground
  - `git-ai bg start` to daemonize (fork + exit parent)
  - `git-ai bg stop` to send SIGTERM via PID file

- [ ] **1.2 Trace2 socket listener** — `src/daemon/trace2_listener.rs`
  - Bind AF_UNIX stream socket at `~/.git-ai/internal/daemon/trace2.sock`
  - Accept connections, read newline-delimited JSON events
  - Handle Unix socket path length limit (>= 100 bytes → hash to /tmp/)
  - Windows: named pipe (`\\.\pipe\git-ai-<hash>-trace2`)

- [ ] **1.3 Trace2 event detection** — `src/daemon/trace2_events.rs`
  - Parse minimal trace2 JSON: extract `event`, `sid`, `argv`, `cmd_name`
  - Detect root-level `exit` event for `git commit` commands
  - Extract working directory (repo path) from `def_repo` or `worktree` events
  - Detect exit code (only process exit_code == 0 commits)

- [ ] **1.4 Post-commit trigger**
  - On successful `git commit` exit: resolve git_dir, HEAD, parent
  - Call existing `generate_authorship_for_commit()` from `core::post_commit`
  - Write authorship note via `git notes --ref=ai add`
  - Write INITIAL attributions for next commit
  - Clean up working log for consumed base commit
  - Skip if commit already has an authorship note (idempotency)

- [ ] **1.5 Install command wires trace2 config**
  - `git-ai install` sets `git config --global trace2.eventTarget af_unix:stream:<path>`
  - `git-ai install` sets `git config --global trace2.eventNesting 10`
  - Disable trace2 in the daemon's own git subprocess calls (`GIT_TRACE2_EVENT=0`)

- [ ] **1.6 Integration tests for daemon mode**
  - Test: daemon detects commit and writes authorship note without hook
  - Test: daemon is idempotent (re-running on same commit is no-op)
  - Test: daemon handles rapid sequential commits
  - Test: graceful shutdown mid-processing

### Phase 2: Control socket (checkpoint ingestion from agents)

Allow AI coding agents to submit checkpoints via the control socket
instead of spawning `git-ai checkpoint` as a subprocess.

- [ ] **2.1 Control socket listener** — `src/daemon/control_socket.rs`
  - AF_UNIX stream socket at `~/.git-ai/internal/daemon/control.sock`
  - Simple request/response protocol: JSON-line request → JSON-line response
  - Connection timeout (30s idle → close)

- [ ] **2.2 Control protocol** — `src/daemon/protocol.rs`
  - `CheckpointRun { repo_dir, kind, entries, agent_id, ... }` → processes checkpoint
  - `Status { repo_dir }` → returns current working log state for repo
  - `Shutdown` → graceful daemon stop
  - `Ping` → health check / version response

- [ ] **2.3 Checkpoint processing via control socket**
  - Reuse the same `update_attributions` + `append_checkpoint` logic
  - But avoid re-reading file from disk (agent sends content in request)
  - Return processed entry count in response

- [ ] **2.4 Agent presets migrate to control socket**
  - Agents that previously called `git-ai checkpoint <agent>` can now send
    a control request directly (lower latency, no subprocess spawn)
  - Fallback: if daemon is not running, fall back to CLI binary call

### Phase 3: Rewrite tracking (rebase, cherry-pick, amend, reset)

Detect history-rewriting operations and propagate authorship notes
to rewritten commits.

- [ ] **3.1 Detect rewrite operations from trace2**
  - `git rebase` — detect start/completion, map old→new commits
  - `git commit --amend` — map previous HEAD to new HEAD
  - `git cherry-pick` — copy authorship from source commit
  - `git reset` — reconstruct working logs from reset commits

- [ ] **3.2 Rewrite log** — `src/daemon/rewrite_log.rs`
  - Record old_sha → new_sha mappings in `.git/ai/rewrite_log`
  - On rewrite completion: copy/adapt authorship notes to new commits
  - Use `git range-diff` or reflog to determine mappings

- [ ] **3.3 Note propagation**
  - Copy authorship note from old commit to new commit
  - Adjust line numbers if rebase introduced conflicts/changes
  - Handle squash (merge multiple notes into one)

### Phase 4: Multi-repo coordination

Track multiple repositories simultaneously.

- [ ] **4.1 Per-repo state isolation**
  - Key all state by repo working directory or git common-dir
  - Concurrent commits in different repos processed independently
  - Single daemon handles all repos on the machine

- [ ] **4.2 Repo discovery from trace2 events**
  - Extract repo path from `def_repo` event or `worktree` field
  - Resolve symlinks, worktrees, gitdir references
  - Cache resolved paths

### Phase 5: Outbound telemetry (metrics, CAS, error reporting)

The daemon sends data to the git-ai backend. This is how the service
tracks usage and provides dashboards. The contract MUST match v1.

- [ ] **5.1 API client** — `src/daemon/api_client.rs`
  - HTTP client (ureq or reqwest-blocking) for outbound calls
  - Base URL from config (`https://usegitai.com` default)
  - Auth: API key or login token from `~/.git-ai/config.json`
  - Retry logic (1 retry after 60s for transient failures)

- [ ] **5.2 Metrics upload** — `POST /worker/metrics/upload`
  - Wire format: `MetricsBatch { v: 1, events: [MetricEvent, ...] }`
  - `MetricEvent`: `{ t: unix_secs, e: event_id, v: sparse_values, a: sparse_attrs }`
  - Events emitted: checkpoint processed, commit attributed, daemon lifecycle
  - Batch flush every 3 seconds (same as v1)
  - Fallback: store in local SQLite if upload fails

- [ ] **5.3 CAS upload** — `POST /worker/cas/upload`
  - Content-addressable store for authorship log snapshots
  - `CasUploadRequest { objects: [{ content: JSON, hash: sha256, metadata: {} }] }`
  - Upload in chunks of 50 objects max
  - Delete from local queue on successful upload

- [ ] **5.4 Error reporting (Sentry + PostHog)**
  - Sentry DSN from config for error/panic reporting
  - PostHog for product analytics events
  - Same envelope format as v1 (`TelemetryEnvelope::Error/Performance/Message`)

- [ ] **5.5 Contract tests**
  - Capture actual v1 outbound HTTP request bodies (record from live daemon)
  - Replay same scenarios in v2, assert request bodies match schema
  - Key invariants to test:
    - MetricsBatch version field = 1
    - MetricEvent fields use compact single-char keys (`t`, `e`, `v`, `a`)
    - CAS hash is SHA256 of content JSON
    - Auth headers present when logged in
    - Retry behavior on 5xx responses

### Phase 6: Robustness and production hardening

- [ ] **6.1 Crash recovery**
  - On startup: scan for orphaned working logs, re-process if needed
  - Stale lock file detection (PID no longer alive → break lock)
  - Socket file cleanup on startup

- [ ] **6.2 Self-update and restart**
  - Periodic version check (configurable interval)
  - Graceful restart: finish in-flight work, re-exec new binary
  - Max uptime guard (restart after ~24h to pick up updates)

- [ ] **6.3 Performance**
  - Batch trace2 events (don't wake per-line, buffer per-connection)
  - Debounce rapid commits (e.g., rebase producing many commits)
  - Async I/O for socket handling (tokio or polling-based)

- [ ] **6.4 Observability**
  - Structured log output (JSON optional)
  - Metrics: commits processed, checkpoints ingested, errors
  - `git-ai bg status` shows uptime, repos tracked, queue depth

---

## Design Decisions

### Sync vs Async

Phase 1 can be **synchronous** (threads for trace listener + main loop).
V1 uses tokio, but the daemon's hot path is I/O-bound (socket reads, git
subprocess calls), not CPU-bound. A thread-per-connection model with
blocking I/O is simpler and sufficient for Phase 1-2.

Migrate to async (tokio) only if connection count or throughput demands it
(Phase 5). Keep the option open by isolating I/O behind trait boundaries.

### Dependencies to add

Phase 1 requires:
- No new deps for basic Unix sockets (`std::os::unix::net`)
- `nix` or raw `libc` for daemonization (fork, setsid)
- `tracing` + `tracing-subscriber` for structured logging (optional, can use eprintln initially)

Phase 2+:
- Potentially `tokio` if async becomes necessary
- `interprocess` for cross-platform named pipes (Windows support)

### Relationship to existing CLI commands

| Command | With daemon running | Without daemon |
|---------|-------------------|----------------|
| `git-ai checkpoint` | Writes to working_log (same as now) | Same |
| `git-ai post-commit` | Writes note (same as now) | Same |
| `git commit` | Daemon auto-detects and writes note | No note unless hook installed |
| `git-ai install` | Starts daemon + sets trace2 config | Sets trace2 config only |

The daemon is additive — all existing CLI paths remain functional.

### Socket path conventions

```
~/.git-ai/internal/daemon/
├── daemon.lock          # flock-based single-instance guard
├── daemon.pid.json      # { "pid": N, "started_at": "...", "version": "..." }
├── daemon.log           # stderr redirect
├── trace2.sock          # AF_UNIX trace2 listener
└── control.sock         # AF_UNIX control API
```

If socket path exceeds Unix limit (108 bytes), hash to:
`/tmp/git-ai-d-<sha256[..16]>/trace.sock`

---

## Current Status

- [x] Core attribution engine (`src/core/attribution.rs`)
- [x] Post-commit authorship generation (`src/core/post_commit.rs`)
- [x] Working log read/write (`src/core/working_log.rs`)
- [x] Authorship log serialization (`src/core/authorship_log.rs`)
- [x] CLI: `git-ai checkpoint` command
- [x] CLI: `git-ai post-commit` command
- [x] CLI: `git-ai blame` command
- [x] CLI: `git-ai stats` command
- [x] CLI: `git-ai install` (basic hook installer)
- [x] Integration test suite (48/48 passing)
- [ ] **Daemon Phase 1** ← next

---

## Outbound Data Contract (v1 compatibility)

V1 daemon sends three categories of outbound data. V2 MUST produce
identical wire formats so the backend doesn't need changes.

### 1. Metrics (`POST /worker/metrics/upload`)

```json
{
  "v": 1,
  "events": [
    {
      "t": 1715644800,       // unix timestamp (u32)
      "e": 42,              // event type ID (u16)
      "v": [[0, 5], [2, 1]], // sparse array of values (position-encoded)
      "a": [[0, "cursor"], [1, "session_abc"]]  // sparse array of attrs
    }
  ]
}
```

Event IDs are defined in `src/metrics/events.rs`. Key ones:
- Agent usage (checkpoint processed)
- Commit attribution generated
- Daemon lifecycle (start/stop/crash)

### 2. CAS (`POST /worker/cas/upload`)

```json
{
  "objects": [
    {
      "content": { /* authorship log or transcript JSON */ },
      "hash": "sha256hex...",
      "metadata": { "repo": "...", "commit": "..." }
    }
  ]
}
```

CAS stores authorship logs and transcripts for the web dashboard.
The hash is SHA256 of the JSON-serialized `content` field.

### 3. Error reporting (Sentry envelope + PostHog capture)

Sentry: standard envelope format to `/api/<project_id>/store/`
PostHog: `POST /capture/` with `{ api_key, distinct_id, event, properties }`

### Testing strategy for outbound contract

1. **Record fixtures**: Run v1 daemon with HTTP intercept, capture request
   bodies for: single commit, multi-file commit, checkpoint ingestion,
   daemon start/stop cycle.

2. **Schema validation**: Define JSON schemas for each endpoint's request
   body. Both v1 and v2 must validate against the same schemas.

3. **Snapshot tests**: v2 generates the same MetricEvent/CAS payload for
   identical input scenarios. Use insta snapshots to detect drift.

4. **Integration test with mock server**: Spin up a local HTTP server in
   tests, configure v2 daemon to point at it, verify request format.

---

## Decisions (resolved)

1. **Daemon is a subcommand** (`git-ai bg run`, `git-ai bg start`, `git-ai bg stop`).
   Single binary, no separate daemon executable.

2. **No tokio unless forced.** Use std threads + blocking I/O (`std::os::unix::net`).
   Only add tokio if a concrete phase demands it — not speculatively.

3. **Checkpoints: both CLI and control socket.** CLI remains primary.
   Control socket is an optimization (Phase 2) for lower-latency agent integration.

4. **V2 kills v1 on install.** `git-ai install` stops the v1 daemon
   (sends shutdown to v1's control socket or kills by PID file), reconfigures
   trace2 to point at v2's socket, and starts v2 daemon. No coexistence.
