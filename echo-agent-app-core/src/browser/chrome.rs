use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::{BrowserError, BrowserResult};

pub const CHROME_BRIDGE_PROTOCOL: u32 = 1;
pub const CHROME_NATIVE_HOST_NAME: &str = "com.eko.browser_bridge";
const MAX_BRIDGE_MESSAGE_BYTES: usize = 1024 * 1024;
type ChromeResponseSender = oneshot::Sender<Result<Value, String>>;
type PendingRequests = Arc<Mutex<HashMap<String, ChromeResponseSender>>>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChromeBridgeEndpoint {
    pub protocol: u32,
    pub port: u16,
    pub token: String,
    pub extension_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChromeBridgeStatus {
    pub enabled: bool,
    pub connected: bool,
    pub extension_origin: Option<String>,
    pub endpoint_file: PathBuf,
    pub startup_error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChromeBridgeHello {
    protocol: u32,
    token: String,
    origin: String,
}

#[derive(Debug, Deserialize)]
struct ChromeBridgeResponse {
    id: String,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

struct ChromeConnection {
    sender: mpsc::Sender<Value>,
    pending: PendingRequests,
    origin: String,
}

#[derive(Clone)]
pub struct ChromeConnectionManager {
    enabled: bool,
    endpoint_file: PathBuf,
    endpoint: Option<ChromeBridgeEndpoint>,
    connection: Arc<RwLock<Option<ChromeConnection>>>,
    shutdown: CancellationToken,
    startup_error: Option<String>,
}

impl ChromeConnectionManager {
    pub async fn start(enabled: bool, bridge_dir: PathBuf, extension_id: Option<String>) -> Self {
        let endpoint_file = bridge_dir.join("endpoint.json");
        let connection = Arc::new(RwLock::new(None));
        let shutdown = CancellationToken::new();
        if !enabled {
            return Self {
                enabled,
                endpoint_file,
                endpoint: None,
                connection,
                shutdown,
                startup_error: None,
            };
        }

        let listener = match TcpListener::bind(("127.0.0.1", 0)).await {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(error = %error, "failed to start Chrome bridge listener");
                let startup_error = format!("failed to start listener: {error}");
                return Self {
                    enabled: false,
                    endpoint_file,
                    endpoint: None,
                    connection,
                    shutdown,
                    startup_error: Some(startup_error),
                };
            }
        };
        let port = match listener.local_addr() {
            Ok(address) => address.port(),
            Err(error) => {
                tracing::warn!(error = %error, "failed to inspect Chrome bridge listener");
                let startup_error = format!("failed to inspect listener: {error}");
                return Self {
                    enabled: false,
                    endpoint_file,
                    endpoint: None,
                    connection,
                    shutdown,
                    startup_error: Some(startup_error),
                };
            }
        };
        let endpoint = ChromeBridgeEndpoint {
            protocol: CHROME_BRIDGE_PROTOCOL,
            port,
            token: uuid::Uuid::new_v4().to_string(),
            extension_id: extension_id.filter(|value| !value.trim().is_empty()),
        };
        if let Err(error) = write_endpoint(&endpoint_file, &endpoint).await {
            tracing::warn!(error = %error, "failed to publish Chrome bridge endpoint");
            let startup_error = format!("failed to publish endpoint: {error}");
            return Self {
                enabled: false,
                endpoint_file,
                endpoint: None,
                connection,
                shutdown,
                startup_error: Some(startup_error),
            };
        }

        let expected = endpoint.clone();
        let connection_slot = connection.clone();
        let accept_shutdown = shutdown.clone();
        tokio::spawn(async move {
            accept_connections(listener, expected, connection_slot, accept_shutdown).await;
        });
        Self {
            enabled,
            endpoint_file,
            endpoint: Some(endpoint),
            connection,
            shutdown,
            startup_error: None,
        }
    }

    pub async fn status(&self) -> ChromeBridgeStatus {
        let connection = self.connection.read().await;
        ChromeBridgeStatus {
            enabled: self.enabled,
            connected: connection.is_some(),
            extension_origin: connection.as_ref().map(|value| value.origin.clone()),
            endpoint_file: self.endpoint_file.clone(),
            startup_error: self.startup_error.clone(),
        }
    }

    pub async fn claim_tab(
        &self,
        conversation_id: &str,
        tab_id: Option<u64>,
    ) -> BrowserResult<Value> {
        self.request(
            "claim_tab",
            json!({
                "conversationId": conversation_id,
                "tabId": tab_id,
            }),
        )
        .await
    }

    pub async fn release_task(&self, conversation_id: &str) -> BrowserResult<Value> {
        self.request("release_task", json!({ "conversationId": conversation_id }))
            .await
    }

    pub async fn browser_action(
        &self,
        conversation_id: &str,
        method: &str,
        params: Value,
    ) -> BrowserResult<Value> {
        self.request(
            method,
            json!({
                "conversationId": conversation_id,
                "params": params,
            }),
        )
        .await
    }

    pub async fn cdp_command(
        &self,
        conversation_id: &str,
        command: &str,
        parameters: Value,
    ) -> BrowserResult<Value> {
        self.browser_action(
            conversation_id,
            "cdp_command",
            json!({
                "command": command,
                "parameters": parameters,
            }),
        )
        .await
    }

    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        *self.connection.write().await = None;
        if let Err(error) = tokio::fs::remove_file(&self.endpoint_file).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(error = %error, "failed to remove Chrome bridge endpoint");
        }
    }

    pub fn endpoint(&self) -> Option<&ChromeBridgeEndpoint> {
        self.endpoint.as_ref()
    }

    async fn request(&self, method: &str, params: Value) -> BrowserResult<Value> {
        if !self.enabled {
            return Err(BrowserError::Connection(
                "Chrome extension bridge is disabled".to_string(),
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (response_sender, response_receiver) = oneshot::channel();
        let sender = {
            let connection = self.connection.read().await;
            let connection = connection.as_ref().ok_or_else(|| {
                BrowserError::Connection(
                    "Chrome extension is not connected; install the extension and native host"
                        .to_string(),
                )
            })?;
            connection
                .pending
                .lock()
                .await
                .insert(id.clone(), response_sender);
            connection.sender.clone()
        };
        let message = json!({
            "id": id,
            "method": method,
            "params": params,
        });
        if sender.send(message).await.is_err() {
            self.remove_pending(&id).await;
            return Err(BrowserError::Connection(
                "Chrome extension connection closed while sending request".to_string(),
            ));
        }
        match tokio::time::timeout(Duration::from_secs(30), response_receiver).await {
            Ok(Ok(Ok(result))) => Ok(result),
            Ok(Ok(Err(error))) => Err(BrowserError::Tool {
                tool: method.to_string(),
                message: error,
            }),
            Ok(Err(_)) => Err(BrowserError::Connection(
                "Chrome extension response channel closed".to_string(),
            )),
            Err(_) => {
                self.remove_pending(&id).await;
                Err(BrowserError::Tool {
                    tool: method.to_string(),
                    message: "Chrome extension action timed out".to_string(),
                })
            }
        }
    }

    async fn remove_pending(&self, id: &str) {
        if let Some(connection) = self.connection.read().await.as_ref() {
            connection.pending.lock().await.remove(id);
        }
    }
}

async fn accept_connections(
    listener: TcpListener,
    expected: ChromeBridgeEndpoint,
    connection_slot: Arc<RwLock<Option<ChromeConnection>>>,
    shutdown: CancellationToken,
) {
    loop {
        let accepted = tokio::select! {
            _ = shutdown.cancelled() => break,
            accepted = listener.accept() => accepted,
        };
        let (stream, _) = match accepted {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(error = %error, "Chrome bridge accept failed");
                continue;
            }
        };
        match establish_connection(stream, &expected, connection_slot.clone()).await {
            Ok(()) => tracing::info!("Chrome extension native bridge connected"),
            Err(error) => tracing::warn!(error = %error, "rejected Chrome bridge connection"),
        }
    }
}

async fn establish_connection<T>(
    stream: T,
    expected: &ChromeBridgeEndpoint,
    connection_slot: Arc<RwLock<Option<ChromeConnection>>>,
) -> BrowserResult<()>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(stream);
    let hello_bytes = read_bounded_line(&mut reader).await?;
    let hello: ChromeBridgeHello = serde_json::from_slice(&hello_bytes).map_err(|error| {
        BrowserError::Connection(format!("invalid Chrome bridge handshake: {error}"))
    })?;
    if hello.protocol != CHROME_BRIDGE_PROTOCOL || hello.token != expected.token {
        return Err(BrowserError::Connection(
            "Chrome bridge handshake authentication failed".to_string(),
        ));
    }
    if !hello.origin.starts_with("chrome-extension://") || !hello.origin.ends_with('/') {
        return Err(BrowserError::Connection(
            "Chrome bridge origin is not a Chrome extension".to_string(),
        ));
    }
    if let Some(extension_id) = expected.extension_id.as_deref()
        && hello.origin != format!("chrome-extension://{extension_id}/")
    {
        return Err(BrowserError::Connection(
            "Chrome bridge extension origin is not authorized".to_string(),
        ));
    }

    let (sender, mut outbound) = mpsc::channel::<Value>(64);
    let pending = Arc::new(Mutex::new(HashMap::<
        String,
        oneshot::Sender<Result<Value, String>>,
    >::new()));
    let writer_task = tokio::spawn(async move {
        while let Some(message) = outbound.recv().await {
            let bytes = match serde_json::to_vec(&message) {
                Ok(bytes) if bytes.len() <= MAX_BRIDGE_MESSAGE_BYTES => bytes,
                Ok(_) => continue,
                Err(_) => continue,
            };
            if writer.write_all(&bytes).await.is_err() || writer.write_all(b"\n").await.is_err() {
                break;
            }
        }
    });
    *connection_slot.write().await = Some(ChromeConnection {
        sender,
        pending: pending.clone(),
        origin: hello.origin,
    });

    loop {
        let bytes = match read_bounded_line(&mut reader).await {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::debug!(error = %error, "Chrome bridge reader stopped");
                break;
            }
        };
        let response = match serde_json::from_slice::<ChromeBridgeResponse>(&bytes) {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(error = %error, "ignored invalid Chrome bridge response");
                continue;
            }
        };
        if let Some(sender) = pending.lock().await.remove(&response.id) {
            let value = match response.error {
                Some(error) => Err(error),
                None => Ok(response.result.unwrap_or(Value::Null)),
            };
            let _ = sender.send(value);
        }
    }
    writer_task.abort();
    let mut slot = connection_slot.write().await;
    if slot
        .as_ref()
        .is_some_and(|connection| Arc::ptr_eq(&connection.pending, &pending))
    {
        *slot = None;
    }
    let mut pending = pending.lock().await;
    for (_, sender) in pending.drain() {
        let _ = sender.send(Err("Chrome extension disconnected".to_string()));
    }
    Ok(())
}

async fn read_bounded_line(reader: &mut (impl AsyncRead + Unpin)) -> BrowserResult<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        let byte = reader
            .read_u8()
            .await
            .map_err(|error| BrowserError::Connection(error.to_string()))?;
        if byte == b'\n' {
            return Ok(bytes);
        }
        if bytes.len() >= MAX_BRIDGE_MESSAGE_BYTES {
            return Err(BrowserError::Connection(
                "Chrome bridge message exceeds 1 MiB".to_string(),
            ));
        }
        bytes.push(byte);
    }
}

async fn write_endpoint(path: &Path, endpoint: &ChromeBridgeEndpoint) -> BrowserResult<()> {
    let parent = path.parent().ok_or_else(|| {
        BrowserError::Connection("Chrome bridge endpoint has no parent directory".to_string())
    })?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| BrowserError::Connection(error.to_string()))?;
    let bytes = serde_json::to_vec_pretty(endpoint)
        .map_err(|error| BrowserError::Connection(error.to_string()))?;
    tokio::fs::write(path, bytes)
        .await
        .map_err(|error| BrowserError::Connection(error.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::Permissions::from_mode(0o600);
        tokio::fs::set_permissions(path, permissions)
            .await
            .map_err(|error| BrowserError::Connection(error.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn endpoint_file_is_private_and_roundtrips() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let path = dir.path().join("endpoint.json");
        let endpoint = ChromeBridgeEndpoint {
            protocol: CHROME_BRIDGE_PROTOCOL,
            port: 41234,
            token: "token".to_string(),
            extension_id: Some("abcdefghijklmnopabcdefghijklmnop".to_string()),
        };
        write_endpoint(&path, &endpoint)
            .await
            .map_err(|error| error.to_string())?;
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|error| error.to_string())?;
        let decoded: ChromeBridgeEndpoint =
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        assert_eq!(decoded, endpoint);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = tokio::fs::metadata(&path)
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
        Ok(())
    }

    #[tokio::test]
    async fn authenticated_bridge_routes_request_and_response() -> Result<(), String> {
        let expected = ChromeBridgeEndpoint {
            protocol: CHROME_BRIDGE_PROTOCOL,
            port: 0,
            token: "bridge-token".to_string(),
            extension_id: Some("abcdefghijklmnopabcdefghijklmnop".to_string()),
        };
        let connection = Arc::new(RwLock::new(None));
        let manager = ChromeConnectionManager {
            enabled: true,
            endpoint_file: PathBuf::from("unused"),
            endpoint: Some(expected.clone()),
            connection: connection.clone(),
            shutdown: CancellationToken::new(),
            startup_error: None,
        };
        let (desktop_stream, extension_stream) = tokio::io::duplex(16 * 1024);
        let desktop = tokio::spawn(async move {
            establish_connection(desktop_stream, &expected, connection)
                .await
                .map_err(|error| error.to_string())
        });
        let fake_extension = tokio::spawn(async move {
            let (mut reader, mut writer) = tokio::io::split(extension_stream);
            let hello = serde_json::to_vec(&json!({
                "protocol": CHROME_BRIDGE_PROTOCOL,
                "token": "bridge-token",
                "origin": "chrome-extension://abcdefghijklmnopabcdefghijklmnop/",
            }))
            .map_err(|error| error.to_string())?;
            writer
                .write_all(&hello)
                .await
                .map_err(|error| error.to_string())?;
            writer
                .write_all(b"\n")
                .await
                .map_err(|error| error.to_string())?;
            let request_bytes = read_bounded_line(&mut reader)
                .await
                .map_err(|error| error.to_string())?;
            let request: Value =
                serde_json::from_slice(&request_bytes).map_err(|error| error.to_string())?;
            let id = request
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| "request id is missing".to_string())?;
            if request.get("method").and_then(Value::as_str) != Some("claim_tab") {
                return Err("unexpected Chrome bridge method".to_string());
            }
            let response = serde_json::to_vec(&json!({
                "id": id,
                "result": { "tabId": 42, "url": "https://example.com" },
            }))
            .map_err(|error| error.to_string())?;
            writer
                .write_all(&response)
                .await
                .map_err(|error| error.to_string())?;
            writer
                .write_all(b"\n")
                .await
                .map_err(|error| error.to_string())?;
            Ok::<(), String>(())
        });
        let mut connected = false;
        for _ in 0..50 {
            if manager.status().await.connected {
                connected = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if !connected {
            return Err("fake Chrome extension did not connect".to_string());
        }
        let result = manager
            .claim_tab("conversation", Some(42))
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(result.get("tabId").and_then(Value::as_u64), Some(42));
        fake_extension.await.map_err(|error| error.to_string())??;
        desktop.await.map_err(|error| error.to_string())??;
        Ok(())
    }
}
