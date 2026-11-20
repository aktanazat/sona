//! Outside MCP servers the user lists in Settings › Agents, which Talk to
//! Sona may call read-only over MCP Streamable HTTP. A server's token lives
//! in the keychain under the server's id; settings keep only whether one is
//! saved, so no listing can carry it. A listing names a server's address by
//! scheme and host alone, since a path can carry a key. Only tools the server
//! marks `readOnlyHint: true` are offered, and each stays off until the owner
//! switches it on, because the hint is the server's own claim.

use crate::secrets::{SecretAccount, SecretManager, SecretValue};
use crate::settings;
use reqwest::header::{HeaderValue, ACCEPT, CONTENT_TYPE};
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use specta::Type;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tauri::{AppHandle, Manager, Runtime};
use zeroize::Zeroizing;

const MAX_NAME_CHARS: usize = 40;
const MAX_DESCRIPTION_CHARS: usize = 300;
const MAX_TOKEN_BYTES: usize = 4_096;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_RESULT_BYTES: usize = 6 * 1024;
const MAX_TOOL_PAGES: usize = 8;
const TIMEOUT: Duration = Duration::from_secs(20);
const PROTOCOL_VERSION: &str = "2025-06-18";
const SESSION_HEADER: &str = "mcp-session-id";
const PROTOCOL_HEADER: &str = "mcp-protocol-version";

static CLIENT: LazyLock<Result<Client, reqwest::Error>> = LazyLock::new(|| {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT)
        .build()
});

/// One listed server as settings keep it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Type)]
pub struct AgentMcpServer {
    pub id: String,
    pub name: String,
    pub url: String,
    /// Whether a token for it is in the keychain. The token itself never is
    /// in settings.
    #[serde(default)]
    pub has_token: bool,
    pub added_at: i64,
    /// The tools the owner switched on for Talk to Sona. Every other tool is
    /// off, whatever the server says about it.
    #[serde(default)]
    pub enabled_tools: Vec<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq, Type)]
pub struct McpServerSummary {
    pub id: String,
    pub name: String,
    pub url: String,
    pub has_token: bool,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq, Type)]
pub struct McpToolSummary {
    pub name: String,
    pub description: String,
    pub enabled: bool,
}

/// Scheme, host and port: what a listing may show of a server's address.
fn origin(url: &str) -> String {
    Url::parse(url).map_or_else(|_| String::new(), |url| url.origin().ascii_serialization())
}

impl From<&AgentMcpServer> for McpServerSummary {
    fn from(server: &AgentMcpServer) -> Self {
        Self {
            id: server.id.clone(),
            name: server.name.clone(),
            url: origin(&server.url),
            has_token: server.has_token,
        }
    }
}

fn operation_gate() -> &'static tokio::sync::Mutex<()> {
    static GATE: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));
    &GATE
}

pub(crate) fn list<R: Runtime>(app: &AppHandle<R>) -> Vec<McpServerSummary> {
    settings::get_settings(app)
        .agent_mcp_servers
        .iter()
        .map(McpServerSummary::from)
        .collect()
}

/// The tools `server` marks read-only, each saying whether the owner switched
/// it on.
pub(crate) async fn read_only_tools<R: Runtime>(
    app: &AppHandle<R>,
    server: &str,
) -> Result<Vec<McpToolSummary>, String> {
    let server = find(app, server)?;
    let mut connection = Connection::open(app, &server).await?;
    Ok(connection
        .tools()
        .await?
        .iter()
        .filter_map(|tool| read_only_summary(tool, &server.enabled_tools))
        .collect())
}

/// The tools Talk to Sona may ask the owner to call on `server`.
pub(crate) async fn enabled_tools<R: Runtime>(
    app: &AppHandle<R>,
    server: &str,
) -> Result<Vec<McpToolSummary>, String> {
    let mut tools = read_only_tools(app, server).await?;
    tools.retain(|tool| tool.enabled);
    Ok(tools)
}

/// Switches one of `id`'s tools on or off for Talk to Sona.
async fn set_tool_enabled<R: Runtime>(
    app: &AppHandle<R>,
    id: &str,
    tool: &str,
    enabled: bool,
) -> Result<(), String> {
    let _gate = operation_gate().lock().await;
    let found = settings::update_settings(app, |settings| {
        let server = settings
            .agent_mcp_servers
            .iter_mut()
            .find(|server| server.id == id)?;
        server.enabled_tools.retain(|name| name != tool);
        if enabled {
            server.enabled_tools.push(tool.to_owned());
        }
        Some(())
    })
    .map_err(|_| "The change could not be saved. Try again.".to_owned())?;
    found.ok_or_else(|| "That server is no longer listed.".to_owned())
}

pub(crate) async fn call_read_only<R: Runtime>(
    app: &AppHandle<R>,
    server: &str,
    tool: &str,
    arguments: Value,
) -> Result<String, String> {
    let arguments = match arguments {
        Value::Null => json!({}),
        Value::Object(_) => arguments,
        _ => return Err("A tool's arguments must be an object.".to_owned()),
    };
    let server = find(app, server)?;
    if !server.enabled_tools.iter().any(|name| name == tool) {
        return Err(format!(
            "{tool} on {} is off. Switch it on in Settings › Agents first.",
            server.name
        ));
    }
    let mut connection = Connection::open(app, &server).await?;
    let tools = connection.tools().await?;
    let listed = tools
        .iter()
        .find(|candidate| candidate.get("name").and_then(Value::as_str) == Some(tool))
        .ok_or_else(|| format!("{} has no tool named {tool}.", server.name))?;
    if read_only_summary(listed, &server.enabled_tools).is_none() {
        return Err(format!(
            "{tool} is not marked read-only by {}, so Sona won't call it.",
            server.name
        ));
    }
    let result = connection
        .request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        )
        .await?;
    let text = bounded(result_text(&result));
    if result.get("isError") == Some(&Value::Bool(true)) {
        return Err(format!(
            "{tool} on {} reported an error: {text}",
            server.name
        ));
    }
    Ok(text)
}

fn find<R: Runtime>(app: &AppHandle<R>, server: &str) -> Result<AgentMcpServer, String> {
    let wanted = server.trim();
    let lowered = wanted.to_lowercase();
    settings::get_settings(app)
        .agent_mcp_servers
        .into_iter()
        .find(|listed| listed.id == wanted || listed.name.to_lowercase() == lowered)
        .ok_or_else(|| {
            format!("No outside MCP server is named {wanted}. Add it in Settings › Agents.")
        })
}

/// A tool as Settings › Agents lists it, or nothing when the server does not
/// mark it read-only.
fn read_only_summary(tool: &Value, enabled_tools: &[String]) -> Option<McpToolSummary> {
    if tool.pointer("/annotations/readOnlyHint") != Some(&Value::Bool(true)) {
        return None;
    }
    let name = tool.get("name")?.as_str()?.trim();
    if name.is_empty() {
        return None;
    }
    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    Some(McpToolSummary {
        name: name.to_owned(),
        description: description.chars().take(MAX_DESCRIPTION_CHARS).collect(),
        enabled: enabled_tools.iter().any(|enabled| enabled == name),
    })
}

fn result_text(result: &Value) -> String {
    let parts: Vec<String> = result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|part| match part.get("type").and_then(Value::as_str) {
            Some("text") => part
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            Some("resource") => match part.pointer("/resource/text").and_then(Value::as_str) {
                Some(text) => text.to_owned(),
                None => format!(
                    "[resource {}]",
                    part.pointer("/resource/uri")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                ),
            },
            Some(kind) => format!("[{kind}]"),
            None => String::new(),
        })
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return result
            .get("structuredContent")
            .map(Value::to_string)
            .unwrap_or_default();
    }
    parts.join("\n\n")
}

fn bounded(text: String) -> String {
    if text.len() <= MAX_RESULT_BYTES {
        return text;
    }
    let mut end = MAX_RESULT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[Cut to the first 6 KiB of {} KiB.]",
        &text[..end],
        text.len().div_ceil(1024)
    )
}

/// The trimmed name and address a new server is saved under.
fn validated(
    existing: &[AgentMcpServer],
    name: &str,
    url: &str,
) -> Result<(String, String), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Give the server a name.".to_owned());
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err("Keep the name to 40 characters.".to_owned());
    }
    let lowered = name.to_lowercase();
    if existing
        .iter()
        .any(|listed| listed.name.to_lowercase() == lowered)
    {
        return Err(format!("A server named {name} is already listed."));
    }
    let parsed = Url::parse(url.trim())
        .map_err(|_| "Enter the server's full address, starting with https://.".to_owned())?;
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err("Put the token in the token field, not in the address.".to_owned());
    }
    match (parsed.scheme(), parsed.host_str()) {
        ("https", Some(_)) | ("http", Some("localhost" | "127.0.0.1")) => {
            Ok((name.to_owned(), parsed.to_string()))
        }
        _ => Err(
            "Use an https address. Plain http works only for localhost or 127.0.0.1.".to_owned(),
        ),
    }
}

async fn add<R: Runtime>(
    app: &AppHandle<R>,
    name: &str,
    url: &str,
    token: Option<String>,
) -> Result<Vec<McpServerSummary>, String> {
    let token = token
        .map(Zeroizing::new)
        .filter(|token| !token.trim().is_empty());
    if let Some(token) = &token {
        if crate::portable::is_portable() {
            return Err("A portable copy of Sona keeps no keychain, so it can't save a token. Add the server without one.".to_owned());
        }
        if token.len() > MAX_TOKEN_BYTES || !token.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err("Paste the token without spaces or line breaks.".to_owned());
        }
    }
    let _gate = operation_gate().lock().await;
    let (name, url) = validated(&settings::get_settings(app).agent_mcp_servers, name, url)?;
    let server = AgentMcpServer {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        url,
        has_token: token.is_some(),
        added_at: chrono::Utc::now().timestamp_millis(),
        enabled_tools: Vec::new(),
    };
    let secrets = app.state::<Arc<SecretManager>>();
    if let Some(token) = token {
        secrets
            .replace(account(&server.id)?, token)
            .await
            .map_err(|_| {
                "The token could not be saved in Keychain. Unlock Keychain and try again."
                    .to_owned()
            })?;
    }
    let saved = settings::update_settings(app, |settings| {
        settings.agent_mcp_servers.push(server.clone());
    });
    if saved.is_err() {
        if server.has_token {
            let _ = secrets.remove(account(&server.id)?).await;
        }
        return Err("The server could not be saved. Try again.".to_owned());
    }
    Ok(list(app))
}

async fn remove<R: Runtime>(app: &AppHandle<R>, id: &str) -> Result<Vec<McpServerSummary>, String> {
    let _gate = operation_gate().lock().await;
    let removed = settings::update_settings(app, |settings| {
        let index = settings
            .agent_mcp_servers
            .iter()
            .position(|server| server.id == id)?;
        Some(settings.agent_mcp_servers.remove(index))
    })
    .map_err(|_| "The server could not be removed. Try again.".to_owned())?;
    if removed.is_some_and(|server| server.has_token) {
        app.state::<Arc<SecretManager>>()
            .remove(account(id)?)
            .await
            .map_err(|_| {
                "The server is gone, but Keychain could not delete its token. Unlock Keychain and try again."
                    .to_owned()
            })?;
    }
    Ok(list(app))
}

fn account(id: &str) -> Result<SecretAccount, String> {
    SecretAccount::agent_mcp_server(id).map_err(|_| "The server's id is invalid.".to_owned())
}

/// One MCP session over Streamable HTTP: JSON-RPC posts to one address,
/// answered as JSON or as an event stream, carrying the session id the
/// server handed out.
struct Connection<'a> {
    client: &'static Client,
    url: Url,
    name: &'a str,
    token: Option<SecretValue>,
    session: Option<HeaderValue>,
    protocol: Option<HeaderValue>,
    next_id: u64,
}

impl<'a> Connection<'a> {
    async fn open<R: Runtime>(
        app: &AppHandle<R>,
        server: &'a AgentMcpServer,
    ) -> Result<Connection<'a>, String> {
        let client = CLIENT
            .as_ref()
            .map_err(|_| "A secure connection could not be prepared.".to_owned())?;
        let token = if server.has_token {
            Some(
                app.state::<Arc<SecretManager>>()
                    .resolve(account(&server.id)?)
                    .await
                    .map_err(|_| {
                        format!(
                            "The token for {} is missing or Keychain is locked. Remove the server and add it again.",
                            server.name
                        )
                    })?,
            )
        } else {
            None
        };
        let url = Url::parse(&server.url)
            .map_err(|_| format!("The address saved for {} is not valid.", server.name))?;
        let mut connection = Connection {
            client,
            url,
            name: &server.name,
            token,
            session: None,
            protocol: None,
            next_id: 0,
        };
        let initialized = connection
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "Sona", "version": env!("CARGO_PKG_VERSION") },
                }),
            )
            .await?;
        connection.protocol = initialized
            .get("protocolVersion")
            .and_then(Value::as_str)
            .and_then(|version| HeaderValue::from_str(version).ok());
        connection
            .post(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await?;
        Ok(connection)
    }

    /// Every tool the server lists, read-only or not, across its pages.
    async fn tools(&mut self) -> Result<Vec<Value>, String> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_TOOL_PAGES {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let page = self.request("tools/list", params).await?;
            tools.extend(
                page.get("tools")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .cloned(),
            );
            cursor = page
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
    }

    async fn post(&self, body: &Value) -> Result<reqwest::Response, String> {
        let mut request = self
            .client
            .post(self.url.clone())
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream")
            .json(body);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token.expose());
        }
        if let Some(session) = &self.session {
            request = request.header(SESSION_HEADER, session.clone());
        }
        if let Some(protocol) = &self.protocol {
            request = request.header(PROTOCOL_HEADER, protocol.clone());
        }
        let response = request.send().await.map_err(|error| {
            if error.is_timeout() {
                format!("{} didn't answer within 20 seconds.", self.name)
            } else {
                format!("{} couldn't be reached.", self.name)
            }
        })?;
        match response.status() {
            status if status.is_success() => Ok(response),
            status @ (StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) => Err(format!(
                "{} refused Sona (HTTP {}). Check its token.",
                self.name,
                status.as_u16()
            )),
            status => Err(format!("{} answered HTTP {}.", self.name, status.as_u16())),
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = Value::from(self.next_id);
        let mut response = self
            .post(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;
        if let Some(session) = response.headers().get(SESSION_HEADER) {
            self.session = Some(session.clone());
        }
        let stream = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        let mut body: Vec<u8> = Vec::new();
        let mut reply = None;
        loop {
            let chunk = response.chunk().await.map_err(|error| {
                if error.is_timeout() {
                    format!("{} didn't finish answering within 20 seconds.", self.name)
                } else {
                    format!("{} stopped answering part way.", self.name)
                }
            })?;
            let Some(chunk) = chunk else { break };
            if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(format!(
                    "{} sent more than 256 KiB, so Sona stopped reading.",
                    self.name
                ));
            }
            body.extend_from_slice(&chunk);
            if stream {
                reply = reply_in_events(&String::from_utf8_lossy(&body), &id);
                if reply.is_some() {
                    break;
                }
            }
        }
        if !stream {
            reply = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|message| reply_in(message, &id));
        }
        let reply =
            reply.ok_or_else(|| format!("{} sent an answer Sona couldn't read.", self.name))?;
        if let Some(error) = reply.get("error") {
            let reason = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("no reason given");
            return Err(format!("{} refused {method}: {reason}", self.name));
        }
        reply
            .get("result")
            .cloned()
            .ok_or_else(|| format!("{} sent an answer Sona couldn't read.", self.name))
    }
}

/// The JSON-RPC answer to `id` in one message, or in a batch of them.
fn reply_in(message: Value, id: &Value) -> Option<Value> {
    match message {
        Value::Array(messages) => messages
            .into_iter()
            .find(|message| message.get("id") == Some(id)),
        message if message.get("id") == Some(id) => Some(message),
        _ => None,
    }
}

/// The answer to `id` among the complete events of a `text/event-stream`
/// body; an event still arriving is left for the next read.
fn reply_in_events(body: &str, id: &Value) -> Option<Value> {
    let body = body.replace("\r\n", "\n");
    let complete = &body[..body.rfind("\n\n")?];
    complete.split("\n\n").find_map(|event| {
        let data: Vec<&str> = event
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(|line| line.strip_prefix(' ').unwrap_or(line))
            .collect();
        serde_json::from_str::<Value>(&data.join("\n"))
            .ok()
            .and_then(|message| reply_in(message, id))
    })
}

#[tauri::command]
#[specta::specta]
pub fn agent_mcp_servers_list(app: AppHandle) -> Vec<McpServerSummary> {
    list(&app)
}

#[tauri::command]
#[specta::specta]
pub async fn agent_mcp_server_add(
    app: AppHandle,
    name: String,
    url: String,
    token: Option<String>,
) -> Result<Vec<McpServerSummary>, String> {
    add(&app, &name, &url, token).await
}

#[tauri::command]
#[specta::specta]
pub async fn agent_mcp_server_remove(
    app: AppHandle,
    id: String,
) -> Result<Vec<McpServerSummary>, String> {
    remove(&app, &id).await
}

#[tauri::command]
#[specta::specta]
pub async fn agent_mcp_server_tools(
    app: AppHandle,
    id: String,
) -> Result<Vec<McpToolSummary>, String> {
    read_only_tools(&app, &id).await
}

#[tauri::command]
#[specta::specta]
pub async fn agent_mcp_server_tool_set(
    app: AppHandle,
    id: String,
    tool: String,
    enabled: bool,
) -> Result<(), String> {
    set_tool_enabled(&app, &id, &tool, enabled).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemorySecretBackend;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    type Called = Arc<std::sync::Mutex<Vec<String>>>;

    fn server(name: &str) -> AgentMcpServer {
        AgentMcpServer {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_owned(),
            url: "https://mcp.example.com/mcp".to_owned(),
            has_token: false,
            added_at: 0,
            enabled_tools: Vec::new(),
        }
    }

    #[test]
    fn a_new_server_needs_a_short_unique_name_and_a_safe_address() {
        let listed = [server("Linear")];
        let long = "n".repeat(41);
        let userinfo = ["https://me", ":pw@docs.example.com/mcp"].concat();
        let cases: [(&str, &str, Result<(&str, &str), &str>); 9] = [
            (
                " Docs ",
                "https://docs.example.com/mcp",
                Ok(("Docs", "https://docs.example.com/mcp")),
            ),
            (
                "Local",
                "http://localhost:8787/mcp",
                Ok(("Local", "http://localhost:8787/mcp")),
            ),
            (
                "Loopback",
                "http://127.0.0.1:9/mcp",
                Ok(("Loopback", "http://127.0.0.1:9/mcp")),
            ),
            (
                "   ",
                "https://docs.example.com/mcp",
                Err("Give the server a name."),
            ),
            (
                &long,
                "https://docs.example.com/mcp",
                Err("Keep the name to 40 characters."),
            ),
            (
                "linear",
                "https://other.example.com/mcp",
                Err("A server named linear is already listed."),
            ),
            (
                "Plain",
                "http://docs.example.com/mcp",
                Err("Use an https address. Plain http works only for localhost or 127.0.0.1."),
            ),
            (
                "Userinfo",
                &userinfo,
                Err("Put the token in the token field, not in the address."),
            ),
            (
                "Query",
                "https://docs.example.com/mcp?key=abc",
                Err("Put the token in the token field, not in the address."),
            ),
        ];
        for (name, url, expected) in cases {
            let expected = expected
                .map(|(name, url)| (name.to_owned(), url.to_owned()))
                .map_err(str::to_owned);
            assert_eq!(validated(&listed, name, url), expected, "{name:?} {url:?}");
        }
    }

    #[test]
    fn only_tools_marked_read_only_are_offered() {
        let page: Value = serde_json::from_str(
            r#"{"tools":[
                {"name":"search","description":"Find issues","annotations":{"readOnlyHint":true}},
                {"name":"delete_issue","annotations":{"readOnlyHint":false,"destructiveHint":true}},
                {"name":"create_issue","description":"No annotations"},
                {"name":"hinted_as_text","annotations":{"readOnlyHint":"true"}}
            ]}"#,
        )
        .expect("tools/list fixture");

        let offered: Vec<McpToolSummary> = page["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .filter_map(|tool| read_only_summary(tool, &[]))
            .collect();

        assert_eq!(
            offered,
            [McpToolSummary {
                name: "search".to_owned(),
                description: "Find issues".to_owned(),
                enabled: false,
            }]
        );
    }

    #[test]
    fn a_long_result_is_cut_and_says_so() {
        let text = bounded("é".repeat(4_000));

        assert!(text.len() < MAX_RESULT_BYTES + 64);
        assert!(
            text.ends_with("\n[Cut to the first 6 KiB of 8 KiB.]"),
            "{}",
            &text[text.len() - 60..]
        );
    }

    fn app_with_secrets(
        data_dir: &tempfile::TempDir,
    ) -> (
        tauri::App<tauri::test::MockRuntime>,
        Arc<MemorySecretBackend>,
    ) {
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context.config_mut().identifier = data_dir.path().to_string_lossy().into_owned();
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_store::Builder::default().build())
            .build(context)
            .expect("test app with a store plugin");
        let backend = Arc::new(MemorySecretBackend::new());
        app.manage(Arc::new(SecretManager::with_backend(backend.clone())));
        (app, backend)
    }

    #[tokio::test]
    async fn a_saved_token_reaches_the_keychain_and_no_listing() {
        let data_dir = tempfile::tempdir().expect("temporary app data");
        let (app, backend) = app_with_secrets(&data_dir);
        let token = "mcp-token-never-listed";

        let listed = add(
            app.handle(),
            "Linear",
            "https://mcp.example.com/mcp",
            Some(token.to_owned()),
        )
        .await
        .expect("added");

        let summary = serde_json::to_string(&listed).expect("summary");
        let settings =
            serde_json::to_string(&settings::get_settings(app.handle())).expect("settings");
        assert!(listed[0].has_token);
        assert!(backend.has(&format!("agent_mcp_servers/{}", listed[0].id)));
        assert!(!summary.contains(token), "{summary}");
        assert!(!settings.contains(token));
    }

    #[tokio::test]
    async fn a_listing_shows_only_the_servers_scheme_and_host() {
        let data_dir = tempfile::tempdir().expect("temporary app data");
        let (app, _) = app_with_secrets(&data_dir);

        let listed = add(
            app.handle(),
            "Linear",
            "https://mcp.example.com:8443/k3y-in-path/mcp",
            None,
        )
        .await
        .expect("added");

        assert_eq!(listed[0].url, "https://mcp.example.com:8443");
        assert_eq!(list(app.handle())[0].url, "https://mcp.example.com:8443");
    }

    #[tokio::test]
    async fn removing_a_server_deletes_its_token() {
        let data_dir = tempfile::tempdir().expect("temporary app data");
        let (app, backend) = app_with_secrets(&data_dir);
        let listed = add(
            app.handle(),
            "Linear",
            "https://mcp.example.com/mcp",
            Some("token".to_owned()),
        )
        .await
        .expect("added");

        let left = remove(app.handle(), &listed[0].id).await.expect("removed");

        assert!(left.is_empty());
        assert!(!backend.has(&format!("agent_mcp_servers/{}", listed[0].id)));
    }

    /// One request off a socket: its lower-cased head and its body.
    async fn read_request(socket: &mut tokio::net::TcpStream) -> Option<(String, String)> {
        let mut raw = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let read = socket.read(&mut buffer).await.ok()?;
            if read == 0 {
                return None;
            }
            raw.extend_from_slice(&buffer[..read]);
            let text = String::from_utf8_lossy(&raw).into_owned();
            let Some(split) = text.find("\r\n\r\n") else {
                continue;
            };
            let head = text[..split].to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if raw.len() >= split + 4 + length {
                return Some((head, text[split + 4..split + 4 + length].to_owned()));
            }
        }
    }

    /// The fake server's answer: status, content type, extra headers, body.
    fn answer(
        head: &str,
        message: &Value,
        called: &Called,
    ) -> (&'static str, &'static str, &'static str, String) {
        let id = message["id"].clone();
        let method = message["method"].as_str().unwrap_or_default();
        if method != "initialize" && !head.contains("mcp-session-id: session-1") {
            return ("400 Bad Request", "text/plain", "", String::new());
        }
        match method {
            "initialize" => (
                "200 OK",
                "application/json",
                "Mcp-Session-Id: session-1\r\n",
                json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":PROTOCOL_VERSION,"capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"1"}}}).to_string(),
            ),
            "notifications/initialized" => ("202 Accepted", "text/plain", "", String::new()),
            "tools/list" => (
                "200 OK",
                "application/json",
                "",
                json!({"jsonrpc":"2.0","id":id,"result":{"tools":[
                    {"name":"lookup","description":"Reads","annotations":{"readOnlyHint":true}},
                    {"name":"erase","description":"Deletes"}
                ]}})
                .to_string(),
            ),
            "tools/call" => {
                let tool = message["params"]["name"].as_str().unwrap_or_default().to_owned();
                called.lock().expect("record").push(tool.clone());
                let reply = json!({"jsonrpc":"2.0","id":id,"result":{"content":[
                    {"type":"text","text":format!("{tool} answered {}", message["params"]["arguments"]["q"])}
                ]}});
                (
                    "200 OK",
                    "text/event-stream",
                    "",
                    format!("event: message\ndata: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}}\n\nevent: message\ndata: {reply}\n\n"),
                )
            }
            _ => ("404 Not Found", "text/plain", "", String::new()),
        }
    }

    /// A Streamable HTTP server with one read-only tool and one that is not.
    /// It hands out a session id, refuses later requests without it, answers
    /// `tools/call` as an event stream, and records every tool called.
    async fn fake_mcp() -> (String, Called) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = format!("http://{}/mcp", listener.local_addr().expect("address"));
        let called: Called = Arc::default();
        let record = called.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let record = record.clone();
                tokio::spawn(async move {
                    let Some((head, body)) = read_request(&mut socket).await else {
                        return;
                    };
                    let message: Value = serde_json::from_str(&body).unwrap_or_default();
                    let (status, content_type, extra, payload) = answer(&head, &message, &record);
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        (address, called)
    }

    async fn app_with_fake_server() -> (
        tempfile::TempDir,
        tauri::App<tauri::test::MockRuntime>,
        Called,
        String,
    ) {
        let data_dir = tempfile::tempdir().expect("temporary app data");
        let (app, _) = app_with_secrets(&data_dir);
        let (address, called) = fake_mcp().await;
        let listed = add(app.handle(), "Fake", &address, None)
            .await
            .expect("added");
        let id = listed[0].id.clone();
        (data_dir, app, called, id)
    }

    #[tokio::test]
    async fn check_lists_only_the_read_only_tool() {
        let (_data_dir, app, _, _) = app_with_fake_server().await;

        let tools = read_only_tools(app.handle(), "fake").await.expect("tools");

        assert_eq!(
            tools,
            [McpToolSummary {
                name: "lookup".to_owned(),
                description: "Reads".to_owned(),
                enabled: false,
            }]
        );
    }

    #[tokio::test]
    async fn a_read_only_tool_stays_off_until_the_owner_switches_it_on() {
        let (_data_dir, app, called, id) = app_with_fake_server().await;

        let refused = call_read_only(app.handle(), "Fake", "lookup", json!({ "q": "S-12" })).await;
        let offered_before = enabled_tools(app.handle(), "Fake").await.expect("tools");
        set_tool_enabled(app.handle(), &id, "lookup", true)
            .await
            .expect("switched on");
        let offered_after = enabled_tools(app.handle(), "Fake").await.expect("tools");

        assert_eq!(
            refused,
            Err("lookup on Fake is off. Switch it on in Settings › Agents first.".to_owned())
        );
        assert!(called.lock().expect("record").is_empty());
        assert!(offered_before.is_empty());
        assert_eq!(
            offered_after
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["lookup"]
        );
    }

    #[tokio::test]
    async fn a_read_only_tool_answers_over_the_session() {
        let (_data_dir, app, _, id) = app_with_fake_server().await;
        set_tool_enabled(app.handle(), &id, "lookup", true)
            .await
            .expect("switched on");

        let text = call_read_only(app.handle(), "Fake", "lookup", json!({ "q": "S-12" }))
            .await
            .expect("called");

        assert_eq!(text, "lookup answered \"S-12\"");
    }

    #[tokio::test]
    async fn a_tool_not_marked_read_only_is_never_called() {
        let (_data_dir, app, called, id) = app_with_fake_server().await;
        set_tool_enabled(app.handle(), &id, "erase", true)
            .await
            .expect("switched on");

        let refused = call_read_only(app.handle(), "Fake", "erase", json!({})).await;

        assert_eq!(
            refused,
            Err("erase is not marked read-only by Fake, so Sona won't call it.".to_owned())
        );
        assert!(called.lock().expect("record").is_empty());
    }
}
