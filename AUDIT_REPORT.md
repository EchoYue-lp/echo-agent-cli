# Security & Code Quality Audit Report

**Project:** echo-agent-cli  
**Date:** 2026-05-31  
**Scope:** `src/`, `echo-agent-app-core/`, `echo-agent-grpc/`, `echo-agent-server/`, `config/`, `capabilities/`, `src-tauri/`, `tauri.conf.json`  
**Auditor:** Automated code audit  

---

## Executive Summary

The echo-agent-cli is a Rust-based AI agent with CLI, web (Axum), and desktop (Tauri) interfaces. The audit identified **6 Critical**, **7 High**, **10 Medium**, and **7 Low** severity findings. The most urgent issues are **arbitrary file read/write via Tauri IPC**, a **network-exposed server with auth disabled by default**, and **unrestricted code execution via the sandbox API**. No `unsafe` Rust blocks were found in the project source.

---

## 1. Security Vulnerabilities

### CRITICAL-01: Tauri IPC Arbitrary File Read/Write (No Path Validation)

**Severity:** Critical  
**Files:**  
- `src/tauri/ipc.rs` lines 21-38 (`native_read_file`)  
- `src/tauri/ipc.rs` lines 43-49 (`native_write_file`)  

**Description:** Both `native_read_file` and `native_write_file` Tauri IPC commands accept arbitrary filesystem paths with zero validation. Any file on the system readable by the process can be read, and any file can be overwritten.

**Vulnerable code (`native_read_file`):**
```rust
#[tauri::command]
pub async fn native_read_file(path: String) -> Result<FileReadResult, String> {
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err(format!("File not found: {}", path));
    }
    // NO path validation — reads ANY file including /etc/shadow, SSH keys, etc.
    let content = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
    // ...
}
```

**Vulnerable code (`native_write_file`):**
```rust
#[tauri::command]
pub async fn native_write_file(path: String, content: String) -> Result<(), String> {
    let p = PathBuf::from(&path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // NO path validation — writes ANY file including /etc/passwd, crontabs, etc.
    std::fs::write(&p, content).map_err(|e| e.to_string())
}
```

**Impact:** A compromised or malicious web frontend (XSS, supply chain attack) can read/write arbitrary files on the user's system, including SSH keys, credentials, and system files. The `native_write_file` also creates arbitrary parent directories.

**Recommendation:** Restrict read/write to a whitelist of allowed directories (e.g., workspace root). Use the existing `validate_path_within_base()` function from `echo-agent-server/src/routes/files.rs` as a reference.

---

### CRITICAL-02: Server Binds to 0.0.0.0 with Auth Disabled by Default

**Severity:** Critical  
**Files:**  
- `config/echo-agent.yaml` line 47: `host: 0.0.0.0`  
- `echo-agent-app-core/src/security/config.rs` line 48: `auth_enabled: false`  

**Description:** The default configuration binds the HTTP server to all network interfaces (`0.0.0.0:3000`) with authentication disabled. This exposes the entire API surface (including code execution, file browsing, terminal, and plugin management) to any device on the local network.

**Default security config:**
```rust
impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            auth_enabled: false,  // <-- AUTH OFF BY DEFAULT
            jwt_secret: String::new(),
            // ...
        }
    }
}
```

**Default server config (echo-agent.yaml):**
```yaml
server:
  host: 0.0.0.0  # <-- ALL INTERFACES
  port: 3000
```

**Impact:** Any device on the same network can access the full API, execute arbitrary code via `/api/sandbox/execute`, browse the filesystem via `/api/files/browse`, install plugins, and interact with the AI agent — all without authentication.

**Recommendation:** Default to `127.0.0.1` binding. Enable auth by default or require explicit opt-in for network exposure.

---

### CRITICAL-03: Sandbox Executes Arbitrary Code with Insufficient Isolation

**Severity:** Critical  
**File:** `echo-agent-server/src/routes/sandbox.rs` lines 104-280  

**Description:** The `/api/sandbox/execute` endpoint executes user-provided shell, Python, or Node.js code with minimal sandboxing. The "High" security tier is only a warning log — there is no container or VM isolation. The `network_enabled` flag is completely ignored (parameter prefixed with `_`).

**Code showing network flag is ignored:**
```rust
async fn execute_local(
    language: &str,
    code: &str,
    _network_enabled: bool,  // <-- UNUSED, network is ALWAYS available
) -> Result<LocalExecuteOutput, String> {
    let (cmd, args) = match language.to_lowercase().as_str() {
        "shell" | "bash" | "sh" => ("sh", vec!["-c".to_string(), code.to_string()]),
        // Code is passed directly to sh -c with no sanitization
```

**Impact:** Given CRITICAL-02 (no auth + network exposure), an attacker on the LAN can execute arbitrary system commands. Even with auth, the sandbox provides no real isolation — code has full network access, filesystem access, and process capabilities.

**Recommendation:** Implement proper sandboxing (seccomp, namespaces, containers). At minimum, enforce `network_enabled` by using network namespaces or firewall rules. Default `auth_enabled: true` and bind to localhost.

---

### CRITICAL-04: Tauri Content Security Policy Disabled

**Severity:** Critical  
**File:** `tauri.conf.json` line 24  

**Description:** The Tauri app has CSP set to `null`, which disables Content Security Policy entirely.

```json
"security": {
  "csp": null
}
```

**Impact:** Without CSP, XSS attacks in the web frontend can execute arbitrary JavaScript, which via Tauri IPC (`native_write_file`, `native_read_file`, and the `shell:default` capability) can fully compromise the system.

**Recommendation:** Set a restrictive CSP that only allows necessary resources.

---

### CRITICAL-05: Tauri Capabilities Grant Shell Access to Frontend

**Severity:** Critical  
**File:** `capabilities/default.json`  

**Description:** The Tauri capability configuration grants `shell:default` and `fs:default` permissions to the web frontend.

```json
{
  "permissions": [
    "core:default",
    "shell:default",   // <-- Shell execution from frontend
    "fs:default",       // <-- Filesystem access from frontend
    "notification:default",
    "global-shortcut:default"
  ]
}
```

Combined with `tauri_plugin_shell::init()` in `src/tauri/mod.rs` line 11, the web frontend has shell execution capabilities.

**Impact:** A compromised frontend (via XSS, especially with CSP disabled per CRITICAL-04) can execute arbitrary shell commands on the host system.

**Recommendation:** Remove `shell:default` and `fs:default` from capabilities. Use only the custom IPC commands with proper path validation.

---

### CRITICAL-06: Tauri App Binds Axum Server to Configured Host (Not Localhost)

**Severity:** Critical  
**File:** `src-tauri/src/main.rs` lines 106-119  

**Description:** The Tauri desktop app starts an Axum HTTP server bound to the configured host/port (from `echo-agent.yaml`, defaulting to `0.0.0.0:3000`), not just localhost.

```rust
let host = &app_config.server.host;
let port = app_config.server.port;
let addr = format!("{}:{}", host, port);
// ...
let listener = tokio::net::TcpListener::bind(&addr).await?;
```

**Impact:** The desktop app exposes the full API on the local network. Combined with auth being disabled by default, this creates the same exposure as CRITICAL-02 in the desktop context.

**Recommendation:** In Tauri mode, always bind to `127.0.0.1` regardless of config.

---

### HIGH-01: Terminal API Accepts Arbitrary Working Directory

**Severity:** High  
**File:** `echo-agent-server/src/routes/terminal.rs` lines 60-85  

**Description:** The `create_terminal` endpoint accepts an arbitrary `cwd` parameter without validating it stays within the workspace boundary.

```rust
pub async fn create(&self, cwd: Option<String>) -> TerminalSession {
    let id = uuid::Uuid::new_v4().to_string();
    let cwd = cwd.unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    });
    // No validation that cwd is within workspace!
```

**Impact:** Can create terminal sessions rooted at any directory on the filesystem (e.g., `/etc`, `/root`, `~/.ssh`).

**Recommendation:** Validate `cwd` against the workspace root using `validate_path_within_base()`.

---

### HIGH-02: File Browser Allows Arbitrary Filesystem Navigation

**Severity:** High  
**File:** `echo-agent-server/src/routes/files.rs` lines 462-519  

**Description:** The `/api/files/browse` endpoint accepts any absolute path without workspace boundary validation. Unlike `list_files` and `read_file` which use `validate_path_within_base()`, `browse_directories` has no such check.

```rust
pub async fn browse_directories(
    Query(params): Query<BrowseParams>,
) -> Response {
    let target = if let Some(ref p) = params.path {
        std::path::PathBuf::from(p)   // <-- Any path accepted
    } else {
        dirs_home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"))
    };
    // No workspace boundary check!
```

**Impact:** Can enumerate the entire filesystem, discovering sensitive files, configurations, and user data.

**Recommendation:** Add workspace boundary validation similar to `list_files`.

---

### HIGH-03: Git Log Command Injection via Unvalidated User Input

**Severity:** High  
**File:** `src/cli/cmd_impls/git.rs` line 161  

**Description:** The `git_log` command passes user-provided `count` argument directly to `git log` without validation.

```rust
async fn git_log(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    // ...
    let count = args.first().copied().unwrap_or("20");
    let log = tokio::process::Command::new("git")
        .args(["log", "--oneline", &format!("-{}", count)])
        // count is not validated — could be "--all" or other git flags
```

**Impact:** A user could pass arbitrary git flags (e.g., `--all --source --remotes`) by manipulating the count argument, potentially exposing unintended git data.

**Recommendation:** Validate that `count` is a positive integer before formatting.

---

### HIGH-04: Plugin Install from Arbitrary Git URLs (Potential SSRF/Code Exec)

**Severity:** High  
**File:** `echo-agent-server/src/routes/plugins.rs` lines 136-161  

**Description:** The `/api/plugins/install` endpoint accepts arbitrary git URLs without validation. A malicious git URL could point to a repository containing harmful plugin code.

```rust
pub async fn install_plugin(
    State(state): State<Arc<AppState>>,
    Json(req): Json<InstallRequest>,
) -> Response {
    let source = InstallSource::parse(&req.source);
    // No URL validation — accepts any git URL including internal network URLs
    match registry.install(&source, scope) {
```

**Impact:** Can clone and execute code from any git repository, including internal network repositories (SSRF). Plugins can contain hooks that execute arbitrary code.

**Recommendation:** Add URL validation (whitelist trusted registries, block internal IPs). Require explicit user confirmation for plugin installation.

---

### HIGH-05: Skills Hub Git Clone Without URL Validation

**Severity:** High  
**File:** `echo-agent-app-core/src/skills_hub/install.rs` lines 54-121  

**Description:** The `install_from_git` function clones arbitrary git URLs without SSRF protection.

```rust
pub async fn install_from_git(
    repo_url: &str,
    subdir: Option<&str>,
    hub: &mut SkillsHub,
) -> Result<InstallResult, String> {
    let output = tokio::process::Command::new("git")
        .args(["clone", "--depth", "1", repo_url, &temp_dir.to_string_lossy()])
        // No URL validation!
```

**Impact:** SSRF via git clone to internal network addresses. Code execution via malicious skill repositories.

**Recommendation:** Apply the same SSRF URL validation used in `validate_webhook_url()` to git URLs.

---

### HIGH-06: Workspace Creation with Arbitrary Root Path

**Severity:** High  
**File:** `echo-agent-server/src/routes/workspace.rs` lines 66-98  

**Description:** The `create_workspace` endpoint accepts an arbitrary root path from the user without validation.

```rust
pub async fn create_workspace(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateWorkspaceRequest>,
) -> Response {
    let result = if let Some(ref root_str) = req.root {
        let root = std::path::PathBuf::from(root_str);
        state.workspace.registry.create_at(&req.name, kind, root)
        // Arbitrary path accepted!
```

**Impact:** Can create workspaces at any filesystem location, potentially overwriting existing data or accessing sensitive directories.

---

### HIGH-07: X-Forwarded-For Header Trusted for Rate Limiting

**Severity:** High  
**File:** `echo-agent-server/src/security_middleware.rs` lines 82-107  

**Description:** The `extract_client_ip` function trusts the `X-Forwarded-For` header for rate limiting. A client can set this header to any value to bypass rate limits.

```rust
fn extract_client_ip(request: &Request) -> String {
    if let Some(forwarded) = request.headers().get("X-Forwarded-For") {
        if let Ok(val) = forwarded.to_str() {
            if let Some(last) = val.rsplit(',').next() {
                // Client can forge this header entirely
```

**Impact:** Rate limiting is trivially bypassable by sending a unique `X-Forwarded-For` header with each request.

**Recommendation:** Only trust proxy headers when behind a known reverse proxy. Use `ConnectInfo<SocketAddr>` as the primary IP source.

---

### MEDIUM-01: JWT Secret Defaults to Empty String

**Severity:** Medium  
**File:** `echo-agent-app-core/src/security/config.rs` line 48  

**Description:** The JWT secret defaults to an empty string. While `validate()` catches this when `auth_enabled` is true, the validation is only a warning log — it does not prevent server startup.

```rust
impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            jwt_secret: String::new(),  // Empty secret
```

**Recommendation:** Generate a random secret at startup if none is configured.

---

### MEDIUM-02: CORS Allows All Origins When None Configured

**Severity:** Medium  
**File:** `echo-agent-server/src/security_middleware.rs` lines 140-153  

**Description:** When `cors_origins` is empty, the CORS layer allows any origin with `AllowOrigin::Any`.

```rust
if security_config.cors_origins.is_empty() {
    CorsLayer::new()
        .allow_origin(Any)  // Allows ALL origins
```

**Impact:** Any website can make cross-origin requests to the API.

---

### MEDIUM-03: Sandbox Security Level Downgrade via API

**Severity:** Medium  
**File:** `echo-agent-server/src/routes/sandbox.rs` lines 86-101  

**Description:** The `PUT /api/sandbox/config` endpoint allows lowering the sandbox security level at runtime.

```rust
pub async fn update_sandbox_config(
    State(state): State<Arc<AppState>>,
    Json(config): Json<SandboxConfig>,
) -> Result<Json<serde_json::Value>, AppError> {
    sandbox_config.security_level = config.security_level;
    // Can downgrade from High to Low!
```

**Recommendation:** Prevent security level downgrades or require re-authentication.

---

### MEDIUM-04: Webhook SSRF Protection Incomplete (DNS Rebinding)

**Severity:** Medium  
**File:** `echo-agent-server/src/routes/webhooks.rs` lines 99-151  

**Description:** The `validate_webhook_url` function checks the hostname at registration time but does not prevent DNS rebinding attacks. An attacker can register a domain that initially resolves to a public IP, then rebind to an internal IP when the webhook fires.

**Impact:** SSRF via DNS rebinding to access internal services.

---

### MEDIUM-05: UTF-8 Truncation in Sandbox Output

**Severity:** Medium  
**File:** `echo-agent-server/src/routes/sandbox.rs` lines 256-263  

**Description:** The output truncation uses byte slicing which can split multi-byte UTF-8 characters.

```rust
let stdout = if stdout.len() > max_len {
    format!("{}...(truncated)", &stdout[..max_len])
    // Byte slice can split a UTF-8 character!
```

**Impact:** Panic or corrupted output when stdout contains multi-byte characters at the truncation boundary. Note: `String::from_utf8_lossy` is used upstream so this is actually safe (lossy output is ASCII-safe), but the pattern is fragile.

---

### MEDIUM-06: PID File Written Without Restrictive Permissions

**Severity:** Medium  
**File:** `echo-agent-app-core/src/server_pid.rs` lines 28-42  

**Description:** The PID file (`~/.echo-agent/server.pid`) is written without setting restrictive file permissions. On multi-user systems, other users can read the port number and potentially connect to the embedded server.

---

### MEDIUM-07: Scheduler Uses block_in_place + block_on (Potential Deadlock)

**Severity:** Medium  
**File:** `echo-agent-app-core/src/scheduler/task.rs` lines 148-153  

**Description:** The `load_from_backend` function uses `tokio::task::block_in_place` with nested `block_on`, which can deadlock if called from a single-threaded tokio runtime.

```rust
fn load_from_backend(&self) -> anyhow::Result<Vec<CronTask>> {
    let store = self.backend.as_ref().unwrap();
    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            store.get(CRON_NAMESPACE, CRON_KEY).await
        })
    });
```

---

### MEDIUM-08: Full MCP Config Replacement via PUT Endpoint

**Severity:** Medium  
**File:** `echo-agent-server/src/routes/mcp.rs` lines 447-519  

**Description:** The `PUT /api/mcp/config` endpoint accepts a complete MCP configuration replacement, allowing an attacker to connect arbitrary stdio/HTTP/SSE MCP servers. While there is command validation for stdio, HTTP/SSE URLs have no SSRF protection.

---

### MEDIUM-09: Web Frontend Has No Origin Validation for WebSocket

**Severity:** Medium  
**File:** `echo-agent-server/src/ws/handler.rs` line 25  

**Description:** The WebSocket endpoint `/ws/chat` does not validate the `Origin` header. Any webpage can open a WebSocket connection to the server.

---

### MEDIUM-10: `expect()` Calls in State Initialization Could Panic

**Severity:** Medium  
**File:** `echo-agent-app-core/src/state.rs` lines 485, 513, 564  

**Description:** Several `expect()` calls in `AppState::from_shared()` will panic if initialization fails.

```rust
.expect("BackgroundTaskService init should not fail")
.expect("in-memory FTS5 engine should always init")
.expect("temp workspace registry should always init")
```

While these have fallback paths, the `expect` on `BackgroundTaskService` has no fallback.

---

## 2. Bugs and Logic Errors

### BUG-01: Silent Error Swallowing in Scratchpad Write

**Severity:** Medium  
**File:** `echo-agent-server/src/routes/scratchpad.rs` lines 52-56  

```rust
std::fs::write(&path, &req.content).ok();  // Error silently ignored
```

If the write fails (disk full, permissions), the API still returns success with the new content, misleading the client.

---

### BUG-02: Error Silently Ignored in Context File Loading

**Severity:** Low  
**File:** `echo-agent-app-core/src/project/context.rs` line 114  

```rust
Err(_) => {}  // Silently swallows read errors
```

File read errors for project context files are silently ignored.

---

### BUG-03: JWT Manager Cache Not Invalidated on Secret Change

**Severity:** Medium  
**File:** `echo-agent-app-core/src/state.rs` lines 661-678  

**Description:** The `get_or_create_jwt_manager` caches the JWT manager. When `reload_security_config` is called to update the JWT secret, the cached manager (built with the old secret) continues to be used. The cache is never invalidated.

```rust
pub async fn reload_security_config(&self) -> Result<(), String> {
    let new_config = SecurityConfig::from_env();
    // jwt_manager cache is NOT cleared!
```

---

### BUG-04: Webhook init_global Uses block_on Inside Async Context

**Severity:** Medium  
**File:** `echo-agent-app-core/src/webhook/emitter.rs` lines 16-23  

```rust
pub fn init_global(endpoints: Vec<WebhookEndpoint>) {
    let rt = tokio::runtime::Handle::current();
    rt.block_on(async {  // block_on inside async context can deadlock
```

---

## 3. Code Quality

### QUALITY-01: TODO Comment in Production Code

**File:** `src/cli/cmd_impls/coding.rs` line 30  
```rust
// TODO: Phase 2 — submit task via BackgroundTaskService
```

### QUALITY-02: `debug_handler` Attribute in Production Routes

**Severity:** Low  
**Files:** Multiple files in `echo-agent-server/src/routes/`  

The `#[debug_handler]` attribute is used extensively. While not harmful in itself, it adds compile-time overhead and should be conditional on debug builds.

### QUALITY-03: Lazy Static Global for Terminal Manager

**Severity:** Low  
**File:** `echo-agent-server/src/routes/terminal.rs` lines 111-113  

```rust
lazy_static::lazy_static! {
    static ref TERMINAL_MANAGER: TerminalManager = TerminalManager::new();
}
```

Using a global static for terminal state means terminal sessions persist across AppState changes and cannot be properly cleaned up on shutdown.

### QUALITY-04: Inconsistent Error Handling Patterns

The codebase mixes several error handling patterns:
- `Result<T, WebError>` in server routes
- `Result<T, String>` in app-core
- `anyhow::Result<T>` in CLI
- Silent `.ok()` swallowing in several places

### QUALITY-05: Redundant `use` Statements in `ws/handler.rs`

**File:** `echo-agent-server/src/ws/handler.rs` line 249  

```rust
use base64::Engine;  // Imported mid-file instead of at top
```

---

## 4. Positive Findings

The following areas show good security practices:

1. **No `unsafe` Rust:** Zero unsafe blocks in the entire project source.
2. **Path traversal protection in file APIs:** `validate_path_within_base()` properly canonicalizes paths and checks boundaries.
3. **MCP command validation:** `validate_mcp_stdio_command()` has a whitelist approach with character-level filtering.
4. **Sensitive file detection:** `sensitive.rs` provides comprehensive pattern matching for secrets/keys.
5. **Webhook SSRF protection:** URL validation blocks localhost, private IPs, and cloud metadata endpoints.
6. **Header redaction in traces:** Authorization and cookie headers are redacted in trace logs.
7. **Constant-time comparison for login:** `constant_time_eq()` prevents timing attacks on username comparison.
8. **bcrypt for password hashing:** Passwords are properly hashed with bcrypt.
9. **File upload path traversal protection:** `save_attachment_to_disk()` validates paths against the upload directory.
10. **kill_on_drop for child processes:** Sandbox execution uses `kill_on_drop(true)` to clean up timed-out processes.

---

## 5. Summary Table

| ID | Severity | Category | Description | File |
|----|----------|----------|-------------|------|
| CRITICAL-01 | Critical | Security | Tauri IPC arbitrary file read/write | `src/tauri/ipc.rs` |
| CRITICAL-02 | Critical | Security | Server on 0.0.0.0 with auth disabled | `config/echo-agent.yaml`, `security/config.rs` |
| CRITICAL-03 | Critical | Security | Sandbox executes arbitrary code | `routes/sandbox.rs` |
| CRITICAL-04 | Critical | Security | Tauri CSP disabled | `tauri.conf.json` |
| CRITICAL-05 | Critical | Security | Tauri shell/fs capabilities | `capabilities/default.json` |
| CRITICAL-06 | Critical | Security | Tauri binds to 0.0.0.0 | `src-tauri/src/main.rs` |
| HIGH-01 | High | Security | Terminal arbitrary cwd | `routes/terminal.rs` |
| HIGH-02 | High | Security | File browser no boundary check | `routes/files.rs` |
| HIGH-03 | High | Security | Git log command injection | `cmd_impls/git.rs` |
| HIGH-04 | High | Security | Plugin install arbitrary URLs | `routes/plugins.rs` |
| HIGH-05 | High | Security | Skills hub git clone SSRF | `skills_hub/install.rs` |
| HIGH-06 | High | Security | Workspace arbitrary root path | `routes/workspace.rs` |
| HIGH-07 | High | Security | X-Forwarded-For spoofing | `security_middleware.rs` |
| MEDIUM-01 | Medium | Security | JWT secret defaults empty | `security/config.rs` |
| MEDIUM-02 | Medium | Security | CORS allows all origins | `security_middleware.rs` |
| MEDIUM-03 | Medium | Security | Sandbox security downgrade | `routes/sandbox.rs` |
| MEDIUM-04 | Medium | Security | Webhook DNS rebinding SSRF | `routes/webhooks.rs` |
| MEDIUM-05 | Medium | Bug | UTF-8 truncation | `routes/sandbox.rs` |
| MEDIUM-06 | Medium | Security | PID file permissions | `server_pid.rs` |
| MEDIUM-07 | Medium | Bug | Scheduler deadlock potential | `scheduler/task.rs` |
| MEDIUM-08 | Medium | Security | MCP config full replacement | `routes/mcp.rs` |
| MEDIUM-09 | Medium | Security | WebSocket no origin check | `ws/handler.rs` |
| MEDIUM-10 | Medium | Bug | expect() panics in init | `state.rs` |
| BUG-01 | Medium | Bug | Scratchpad silent error | `routes/scratchpad.rs` |
| BUG-02 | Low | Bug | Context file error swallowed | `project/context.rs` |
| BUG-03 | Medium | Bug | JWT cache not invalidated | `state.rs` |
| BUG-04 | Medium | Bug | Webhook block_on deadlock | `webhook/emitter.rs` |
| QUALITY-01 | Low | Quality | TODO in production | `cmd_impls/coding.rs` |
| QUALITY-02 | Low | Quality | debug_handler in production | multiple routes |
| QUALITY-03 | Low | Quality | Global static terminal mgr | `routes/terminal.rs` |
| QUALITY-04 | Low | Quality | Inconsistent error handling | codebase-wide |
| QUALITY-05 | Low | Quality | Mid-file use statement | `ws/handler.rs` |

---

## 6. Priority Remediation Order

1. **Immediately** fix CRITICAL-01 (Tauri IPC path validation) and CRITICAL-04/05 (CSP + capabilities)
2. **Change defaults** for CRITICAL-02/06 (bind to 127.0.0.1, enable auth)
3. **Add workspace boundary checks** for HIGH-01, HIGH-02, HIGH-06
4. **Add URL validation** for HIGH-04, HIGH-05 (plugin/skills git install)
5. **Implement proper sandboxing** for CRITICAL-03
6. **Fix JWT cache invalidation** (BUG-03) and rate limiting (HIGH-07)
7. Address remaining Medium and Low findings in subsequent releases
