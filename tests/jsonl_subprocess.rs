use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use echo_agent::agent::AgentEvent;
use echo_agent_app_core::chat_driver::ChatDriverEvent;
use echo_agent_app_core::chat_event_log::ChatEventEnvelope;
use echo_agent_app_core::tasks::task_runtime::RuntimeEventKind;
use echo_agent_app_core::tasks::task_runtime::executor::ExecEventScope;

#[derive(Clone, Copy)]
enum FixtureMode {
    ToolThenAnswer,
    TaskThenAnswer,
    Error,
    #[cfg(unix)]
    Stall,
}

struct FixtureServer {
    address: String,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    task: Option<thread::JoinHandle<Result<()>>>,
}

impl FixtureServer {
    fn start(mode: FixtureMode) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?.to_string();
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let task_requests = Arc::clone(&requests);
        let task_stop = Arc::clone(&stop);
        let task = thread::spawn(move || {
            let mut connections = Vec::new();
            while !task_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let connection_requests = Arc::clone(&task_requests);
                        let connection_stop = Arc::clone(&task_stop);
                        connections.push(thread::spawn(move || {
                            serve_fixture_request(
                                stream,
                                mode,
                                &connection_requests,
                                &connection_stop,
                            )
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            for connection in connections {
                match connection.join() {
                    Ok(result) => result?,
                    Err(_) => return Err(anyhow!("fixture connection task panicked")),
                }
            }
            Ok(())
        });
        Ok(Self {
            address,
            requests,
            stop,
            task: Some(task),
        })
    }

    fn endpoint(&self) -> String {
        format!("http://{}", self.address)
    }

    fn wait_for_requests(&self, expected: usize, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while self.requests.load(Ordering::Acquire) < expected {
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "fixture server observed {} requests, expected {expected}",
                    self.requests.load(Ordering::Acquire)
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    fn shutdown(mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        let task = self
            .task
            .take()
            .ok_or_else(|| anyhow!("fixture server task is unavailable"))?;
        match task.join() {
            Ok(result) => result,
            Err(_) => Err(anyhow!("fixture server task panicked")),
        }
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

struct IsolatedRoot(PathBuf);

impl IsolatedRoot {
    fn new(label: &str) -> Result<Self> {
        let path = std::env::temp_dir().join(format!("eko-jsonl-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for IsolatedRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn serve_fixture_request(
    mut stream: TcpStream,
    mode: FixtureMode,
    requests: &AtomicUsize,
    stop: &AtomicBool,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut request = Vec::new();
    let mut chunk = [0_u8; 4096];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = match stream.read(&mut chunk) {
            Ok(read) => read,
            Err(error)
                if request.is_empty()
                    && matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
            {
                return Ok(());
            }
            Err(error) => {
                return Err(error).context("fixture connection failed while reading HTTP headers");
            }
        };
        if read == 0 {
            return if request.is_empty() {
                Ok(())
            } else {
                Err(anyhow!("fixture client closed during its HTTP headers"))
            };
        }
        let bytes = chunk
            .get(..read)
            .ok_or_else(|| anyhow!("fixture request read exceeded its buffer"))?;
        request.extend_from_slice(bytes);
    }
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .and_then(|position| position.checked_add(4))
        .ok_or_else(|| anyhow!("fixture request omitted its header boundary"))?;
    let headers = request
        .get(..header_end)
        .ok_or_else(|| anyhow!("fixture request header boundary exceeded its buffer"))?;
    let headers = std::str::from_utf8(headers)?.to_string();
    let request_path = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| anyhow!("fixture request omitted its HTTP path"))?
        .to_string();
    let content_length = headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.trim().parse::<usize>())
        .transpose()?
        .unwrap_or_default();
    let request_length = header_end
        .checked_add(content_length)
        .ok_or_else(|| anyhow!("fixture request length overflowed"))?;
    while request.len() < request_length {
        let read = stream.read(&mut chunk).with_context(|| {
            format!(
                "fixture request body read failed at {}/{} bytes; headers: {headers:?}",
                request.len(),
                request_length
            )
        })?;
        if read == 0 {
            return Err(anyhow!(
                "fixture client closed before sending its HTTP body"
            ));
        }
        let bytes = chunk
            .get(..read)
            .ok_or_else(|| anyhow!("fixture request body read exceeded its buffer"))?;
        request.extend_from_slice(bytes);
    }

    if !request_path.ends_with("/chat/completions") {
        return write_json(&mut stream, r#"{"data":[],"object":"list"}"#);
    }
    let body = request
        .get(header_end..request_length)
        .ok_or_else(|| anyhow!("fixture request body boundary exceeded its buffer"))?;
    let body: serde_json::Value = serde_json::from_slice(body)?;
    let uses_tools = body
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|tools| !tools.is_empty());
    if !uses_tools {
        return write_sse(&mut stream, final_answer_sse());
    }
    let request_index = match mode {
        #[cfg(unix)]
        FixtureMode::Stall => {
            stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )?;
            stream.flush()?;
            // The cancellation test treats the request count as its readiness
            // barrier. Publish it only after the streaming response is live.
            requests.fetch_add(1, Ordering::AcqRel);
            while !stop.load(Ordering::Acquire) {
                if stream.write_all(b": fixture keepalive\n\n").is_err() || stream.flush().is_err()
                {
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
            return Ok(());
        }
        _ => requests.fetch_add(1, Ordering::AcqRel),
    };

    match mode {
        FixtureMode::ToolThenAnswer if request_index == 0 => {
            write_sse(&mut stream, tool_call_sse())
        }
        FixtureMode::ToolThenAnswer => write_sse(&mut stream, final_answer_sse()),
        FixtureMode::TaskThenAnswer if request_index == 0 => {
            write_sse(&mut stream, task_create_sse())
        }
        FixtureMode::TaskThenAnswer if request_index == 1 => {
            write_sse(&mut stream, task_execute_sse())
        }
        FixtureMode::TaskThenAnswer if request_index == 2 => {
            write_sse(&mut stream, subagent_answer_sse())
        }
        FixtureMode::TaskThenAnswer => write_sse(&mut stream, final_answer_sse()),
        FixtureMode::Error => write_http_error(&mut stream),
        #[cfg(unix)]
        FixtureMode::Stall => Ok(()),
    }
}

fn write_json(stream: &mut TcpStream, body: &str) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )?;
    stream.flush()?;
    Ok(())
}

fn write_sse(stream: &mut TcpStream, body: String) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )?;
    stream.flush()?;
    Ok(())
}

fn write_http_error(stream: &mut TcpStream) -> Result<()> {
    let body = r#"{"error":{"message":"fixture rejection","type":"invalid_request_error"}}"#;
    write!(
        stream,
        "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )?;
    stream.flush()?;
    Ok(())
}

fn tool_call_sse() -> String {
    function_tool_call_sse(
        "fixture-tool",
        "fixture-call",
        "fixture_missing_tool",
        serde_json::json!({}),
    )
}

fn task_create_sse() -> String {
    function_tool_call_sse(
        "fixture-task-create",
        "fixture-create-plan",
        "task_create",
        serde_json::json!({
            "tasks": [{
                "id": "inspect",
                "title": "Inspect runtime",
                "description": "Inspect the runtime and report evidence",
                "kind": "read_only_review",
                "subagent": "explorer"
            }]
        }),
    )
}

fn task_execute_sse() -> String {
    function_tool_call_sse(
        "fixture-task-execute",
        "fixture-execute-plan",
        "task_execute",
        serde_json::json!({"revision": 1}),
    )
}

fn function_tool_call_sse(
    response_id: &str,
    call_id: &str,
    name: &str,
    arguments: serde_json::Value,
) -> String {
    let arguments = arguments.to_string();
    let delta = serde_json::json!({
        "id": response_id,
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "tool_calls": [{
                    "index": 0,
                    "id": call_id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments}
                }]
            },
            "finish_reason": null
        }]
    });
    let finished = serde_json::json!({
        "id": response_id,
        "choices": [{"index": 0, "delta": null, "finish_reason": "tool_calls"}],
        "usage": {"prompt_tokens": 8, "completion_tokens": 2, "total_tokens": 10}
    });
    [
        format!("data: {delta}"),
        format!("data: {finished}"),
        "data: [DONE]".to_string(),
        String::new(),
    ]
    .join("\n\n")
}

fn subagent_answer_sse() -> String {
    [
        r#"data: {"id":"fixture-subagent-answer","choices":[{"index":0,"delta":{"role":"assistant","content":"inspected runtime evidence"},"finish_reason":null}]}"#,
        r#"data: {"id":"fixture-subagent-answer","choices":[{"index":0,"delta":null,"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":3,"total_tokens":13}}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n")
}

fn final_answer_sse() -> String {
    [
        r#"data: {"id":"fixture-answer","choices":[{"index":0,"delta":{"role":"assistant","content":"fixture answer"},"finish_reason":null}]}"#,
        r#"data: {"id":"fixture-answer","choices":[{"index":0,"delta":null,"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15}}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n")
}

fn fixture_config(root: &IsolatedRoot, endpoint: &str) -> Result<PathBuf> {
    let path = root.path().join("echo-agent.yaml");
    let yaml = format!(
        r#"model:
  default_model_id: fixture:fixture-model
  provider: fixture
  name: fixture-model
  max_tokens: 128
model_providers:
  fixture:
    name: Fixture
    auth_token: fixture-token
    base_url: "{endpoint}"
    default_api_protocol: chat_completions
    requires_api_key: true
configured_models:
  - id: fixture:fixture-model
    display_name: Fixture Model
    provider: fixture
    model: fixture-model
    api_protocol: chat_completions
    input_modalities: [text]
    enabled: true
    max_tokens: 128
    context_window: 128000
agent:
  name: fixture-agent
  system_prompt: Return the requested result.
  max_iterations: 3
  enable_tools: true
  enable_memory: false
  enable_human_in_loop: false
  compress_strategy: ""
logging:
  level: error
"#
    );
    std::fs::write(&path, yaml)?;
    Ok(path)
}

fn spawn_jsonl(root: &IsolatedRoot, endpoint: &str) -> Result<std::process::Child> {
    let config = fixture_config(root, endpoint)?;
    Command::new(env!("CARGO_BIN_EXE_echo-agent-cli"))
        .args([
            "--jsonl",
            "exercise the canonical machine event surface",
            "--config",
        ])
        .arg(config)
        .arg("--project")
        .arg(root.path())
        .env("EKO_DATA_DIR", root.path().join(".eko"))
        .env("HOME", root.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn the JSONL product entry")
}

fn wait_for_output(mut child: std::process::Child, timeout: Duration) -> Result<Output> {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return child
                .wait_with_output()
                .context("failed to collect the JSONL product output");
        }
        if Instant::now() >= deadline {
            child
                .kill()
                .context("failed to stop a timed-out JSONL product entry")?;
            let output = child
                .wait_with_output()
                .context("failed to reap a timed-out JSONL product entry")?;
            return Err(anyhow!(
                "JSONL product entry did not settle within {} seconds; stderr: {}",
                timeout.as_secs(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn run_jsonl(root: &IsolatedRoot, endpoint: &str) -> Result<Output> {
    wait_for_output(spawn_jsonl(root, endpoint)?, Duration::from_secs(30))
}

fn settle_fixture(server: FixtureServer, expected: usize, output: &Output) -> Result<()> {
    let request_result = server.wait_for_requests(expected, Duration::from_secs(1));
    let shutdown_result = server.shutdown();
    match (request_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (requests, shutdown) => Err(anyhow!(
            "fixture settlement failed: requests={requests:?}, shutdown={shutdown:?}; JSONL child exited with {}; stdout: {}; stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )),
    }
}

fn parse_stdout(output: &Output) -> Result<Vec<ChatEventEnvelope>> {
    let stdout = std::str::from_utf8(&output.stdout)?;
    if stdout.lines().next().is_none() {
        return Err(anyhow!("JSONL product entry emitted no envelopes"));
    }
    stdout
        .lines()
        .map(|line| {
            serde_json::from_str::<ChatEventEnvelope>(line)
                .with_context(|| format!("stdout contained non-envelope content: {line}"))
        })
        .collect()
}

fn count_agent_events(
    events: &[ChatEventEnvelope],
    predicate: impl Fn(&AgentEvent) -> bool,
) -> usize {
    events
        .iter()
        .filter(|envelope| {
            matches!(
                &envelope.payload,
                ChatDriverEvent::Agent(agent) if predicate(&agent.payload)
            )
        })
        .count()
}

fn count_turn_status(events: &[ChatEventEnvelope], expected: &str) -> usize {
    events
        .iter()
        .filter(|envelope| {
            matches!(
                &envelope.payload,
                ChatDriverEvent::TurnStatus { status } if status == expected
            )
        })
        .count()
}

fn execution_events(
    events: &[ChatEventEnvelope],
) -> impl Iterator<Item = &echo_agent_app_core::tasks::task_runtime::executor::ExecEvent> {
    events
        .iter()
        .filter_map(|envelope| match &envelope.payload {
            ChatDriverEvent::Execution(event) => Some(event),
            _ => None,
        })
}

fn assert_ordered_single_stream(events: &[ChatEventEnvelope], terminal_status: &str) -> Result<()> {
    let first = events
        .first()
        .ok_or_else(|| anyhow!("JSONL product entry emitted no envelopes"))?;
    let mut expected_sequence = first.sequence;
    let mut event_ids = BTreeSet::new();
    for envelope in events {
        if envelope.stream_id != first.stream_id {
            return Err(anyhow!("JSONL output crossed journal streams"));
        }
        if envelope.sequence != expected_sequence {
            return Err(anyhow!(
                "JSONL sequence jumped from expected {expected_sequence} to {}",
                envelope.sequence
            ));
        }
        if !event_ids.insert(envelope.event_id.as_str()) {
            return Err(anyhow!(
                "JSONL output repeated event id {}",
                envelope.event_id
            ));
        }
        expected_sequence = expected_sequence
            .checked_add(1)
            .ok_or_else(|| anyhow!("JSONL sequence overflowed"))?;
    }
    if !matches!(
        events.last().map(|envelope| &envelope.payload),
        Some(ChatDriverEvent::TurnStatus { status }) if status == terminal_status
    ) {
        return Err(anyhow!(
            "JSONL output did not end with the {terminal_status} terminal status"
        ));
    }
    Ok(())
}

#[test]
fn jsonl_subprocess_emits_rich_tool_facts_and_one_success_terminal_without_stdout_noise()
-> Result<()> {
    let root = IsolatedRoot::new("success")?;
    let server = FixtureServer::start(FixtureMode::ToolThenAnswer)?;
    let output = run_jsonl(&root, &server.endpoint())?;
    settle_fixture(server, 2, &output)?;
    let events = parse_stdout(&output)?;
    assert_ordered_single_stream(&events, "completed")?;

    if !output.status.success() {
        return Err(anyhow!(
            "JSONL success fixture exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let tool_calls = count_agent_events(&events, |event| {
        matches!(event, AgentEvent::ToolCall { .. })
    });
    let failed_results = count_agent_events(
        &events,
        |event| matches!(event, AgentEvent::ToolResult { result, .. } if !result.success && result.failure.is_some()),
    );
    if (tool_calls, failed_results) != (1, 1) {
        return Err(anyhow!(
            "JSONL tool lifecycle was ({tool_calls}, {failed_results}), expected (1, 1); stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ));
    }
    assert_eq!(
        count_agent_events(&events, |event| matches!(event, AgentEvent::FinalAnswer(_))),
        1
    );
    assert_eq!(count_turn_status(&events, "completed"), 1);
    assert_eq!(count_turn_status(&events, "failed"), 0);
    assert_eq!(count_turn_status(&events, "cancelled"), 0);
    Ok(())
}

#[test]
fn jsonl_subprocess_preserves_real_task_plan_and_subagent_lifecycle() -> Result<()> {
    let root = IsolatedRoot::new("task")?;
    let server = FixtureServer::start(FixtureMode::TaskThenAnswer)?;
    let output = run_jsonl(&root, &server.endpoint())?;
    settle_fixture(server, 4, &output)?;
    let events = parse_stdout(&output)?;
    assert_ordered_single_stream(&events, "completed")?;

    if !output.status.success() {
        return Err(anyhow!(
            "JSONL task fixture exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    assert_eq!(
        count_agent_events(&events, |event| {
            matches!(event, AgentEvent::ToolCall { invocation, .. } if invocation.name == "task_create")
        }),
        1
    );
    assert_eq!(
        count_agent_events(&events, |event| {
            matches!(event, AgentEvent::ToolCall { invocation, .. } if invocation.name == "task_execute")
        }),
        1
    );
    assert_eq!(
        count_agent_events(&events, |event| {
            matches!(event, AgentEvent::ToolResult { name, result, .. } if name == "task_create" && result.success)
        }),
        1
    );
    assert_eq!(
        count_agent_events(&events, |event| {
            matches!(event, AgentEvent::ToolResult { name, result, .. } if name == "task_execute" && result.success)
        }),
        1
    );

    let runtime_events = execution_events(&events).collect::<Vec<_>>();
    let first = runtime_events
        .first()
        .ok_or_else(|| anyhow!("task fixture emitted no TaskRuntime events"))?;
    if first.run_id.is_empty()
        || runtime_events
            .iter()
            .any(|event| event.run_id != first.run_id)
    {
        return Err(anyhow!("TaskRuntime events lost or crossed run identity"));
    }
    let task_started = runtime_events
        .iter()
        .position(|event| {
            event.scope == ExecEventScope::Task
                && event.task_id.as_deref() == Some("inspect")
                && event.event == RuntimeEventKind::TaskStarted
        })
        .ok_or_else(|| anyhow!("task fixture omitted the PlanTask start"))?;
    let subagent_started = runtime_events
        .iter()
        .position(|event| {
            event.scope == ExecEventScope::Subagent
                && event.task_id.as_deref() == Some("inspect")
                && event.event == RuntimeEventKind::Started
        })
        .ok_or_else(|| {
            anyhow!(
                "task fixture omitted the SubagentRun start; runtime events: {runtime_events:#?}"
            )
        })?;
    let subagent_run_id = runtime_events
        .get(subagent_started)
        .and_then(|event| event.subagent_run_id.as_deref())
        .ok_or_else(|| anyhow!("SubagentRun start omitted its identity"))?;
    let subagent_completed = runtime_events
        .iter()
        .position(|event| {
            event.scope == ExecEventScope::Subagent
                && event.subagent_run_id.as_deref() == Some(subagent_run_id)
                && event.event == RuntimeEventKind::Completed
        })
        .ok_or_else(|| anyhow!("task fixture omitted the SubagentRun completion"))?;
    let task_completed = runtime_events
        .iter()
        .position(|event| {
            event.scope == ExecEventScope::Task
                && event.task_id.as_deref() == Some("inspect")
                && event.event == RuntimeEventKind::TaskCompleted
        })
        .ok_or_else(|| anyhow!("task fixture omitted the PlanTask completion"))?;
    let run_completed = runtime_events
        .iter()
        .position(|event| {
            event.scope == ExecEventScope::Run && event.event == RuntimeEventKind::RunCompleted
        })
        .ok_or_else(|| anyhow!("task fixture omitted the TaskRun completion"))?;
    assert!(task_started < subagent_started);
    assert!(subagent_started < subagent_completed);
    assert!(subagent_completed <= task_completed);
    assert!(task_completed < run_completed);
    assert_eq!(count_turn_status(&events, "completed"), 1);
    Ok(())
}

#[test]
fn jsonl_subprocess_emits_one_typed_error_and_failed_terminal_without_stdout_noise() -> Result<()> {
    let root = IsolatedRoot::new("error")?;
    let server = FixtureServer::start(FixtureMode::Error)?;
    let output = run_jsonl(&root, &server.endpoint())?;
    settle_fixture(server, 1, &output)?;
    let events = parse_stdout(&output)?;
    assert_ordered_single_stream(&events, "failed")?;

    assert!(!output.status.success());
    assert_eq!(
        count_agent_events(&events, |event| {
            matches!(event, AgentEvent::Error { failure, .. } if !failure.code.is_empty())
        }),
        1
    );
    assert_eq!(count_turn_status(&events, "failed"), 1);
    assert_eq!(count_turn_status(&events, "completed"), 0);
    assert_eq!(count_turn_status(&events, "cancelled"), 0);
    Ok(())
}

#[cfg(unix)]
#[test]
fn jsonl_subprocess_sigint_emits_one_cancelled_fact_and_terminal() -> Result<()> {
    let root = IsolatedRoot::new("cancel")?;
    let server = FixtureServer::start(FixtureMode::Stall)?;
    let mut child = spawn_jsonl(&root, &server.endpoint())?;
    server.wait_for_requests(1, Duration::from_secs(30))?;
    let signal = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .output()
        .context("failed to signal the JSONL product entry")?;
    if !signal.status.success() {
        let _ = child.kill();
        return Err(anyhow!(
            "kill -INT failed: {}",
            String::from_utf8_lossy(&signal.stderr)
        ));
    }
    let output = wait_for_output(child, Duration::from_secs(30))?;
    server.shutdown()?;
    let events = parse_stdout(&output)?;
    assert_ordered_single_stream(&events, "cancelled").with_context(|| {
        format!(
            "SIGINT JSONL stdout: {}; stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })?;

    assert!(!output.status.success());
    assert_eq!(
        count_agent_events(&events, |event| matches!(event, AgentEvent::Cancelled)),
        1
    );
    assert_eq!(count_turn_status(&events, "cancelled"), 1);
    assert_eq!(count_turn_status(&events, "completed"), 0);
    assert_eq!(count_turn_status(&events, "failed"), 0);
    Ok(())
}
