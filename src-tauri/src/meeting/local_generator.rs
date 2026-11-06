use super::processing::{MeetingTextGenerationError, MeetingTextGenerator, ReplyShape};
use crate::llm_client::{
    probe_loopback_model_info, send_loopback_chat_completion, LoopbackChatCompletionError,
    LoopbackModel, LoopbackReply,
};
use crate::settings::{MeetingLocalEngine, PostProcessEndpoint};
use serde::Serialize;
use serde_json::Value;
use specta::Type;
use std::borrow::Cow;
use std::future::Future;
#[cfg(test)]
use std::io::{Read, Write};
#[cfg(test)]
use std::net::TcpListener;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
#[cfg(test)]
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

const LOCAL_ENGINE_MODEL_ID: &str = "local-openai-compatible";
const AVAILABILITY_CACHE_TTL: Duration = Duration::from_secs(2);
const MEETING_REQUEST_TIMEOUT: Duration = Duration::from_secs(240);
/// What one token of a local model's window holds, in bytes of the pack as
/// it is built, before [`SourceLabels`] shortens it. Measured with the
/// qwen3 tokenizer on notes rows: 3.3 bytes a token once the uuids are
/// labels, 2.2 with them. Ollama cuts a prompt that overflows its window
/// without saying so, and the model then answers about what was left.
const LOCAL_CONTEXT_BYTES_PER_TOKEN: usize = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LocalEndpointError {
    InvalidEndpoint,
    Unreachable,
    InvalidResponse,
}

impl LocalEndpointError {
    pub(crate) const fn reason_code(self) -> &'static str {
        match self {
            Self::InvalidEndpoint => "invalid_endpoint",
            Self::Unreachable => "unreachable",
            Self::InvalidResponse => "invalid_response",
        }
    }
}

impl std::fmt::Display for LocalEndpointError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::InvalidEndpoint => "invalid local endpoint",
            Self::Unreachable => "endpoint unreachable",
            Self::InvalidResponse => "endpoint returned an invalid model list",
        };
        formatter.write_str(message)
    }
}

/// Why Apple Intelligence cannot answer right now. Three of these are
/// different instructions rather than three ways of saying "unavailable":
/// a switch to flip, a download to wait for, or hardware that will never
/// qualify. The codes come from `swift/apple_intelligence_bridge.h`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AppleIntelligenceBlocker {
    /// The switch in System Settings > Apple Intelligence & Siri is off.
    NotEnabled,
    /// Switched on, and the model is still downloading.
    ModelNotReady,
    /// This machine will never run it.
    DeviceNotEligible,
    /// macOS older than 26.
    OsTooOld,
    /// A reason this build does not know.
    Unknown,
}

impl std::fmt::Display for AppleIntelligenceBlocker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NotEnabled => {
                "Apple Intelligence is switched off in System Settings > Apple Intelligence & Siri"
            }
            Self::ModelNotReady => "Apple Intelligence is still downloading its model",
            Self::DeviceNotEligible => "this Mac is not eligible for Apple Intelligence",
            Self::OsTooOld => "Apple Intelligence requires macOS 26 or newer",
            Self::Unknown => "Apple Intelligence is unavailable for an unrecognized reason",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeetingLocalEngineStatus {
    AppleIntelligence {
        /// `None` when it can answer.
        blocker: Option<AppleIntelligenceBlocker>,
    },
    LocalEndpoint {
        reachable: bool,
        model_count: usize,
        error: Option<String>,
    },
}

/// Where the next meeting's text would be written, for a series that has not
/// been kept on this Mac. The answer a start page needs before a meeting
/// exists: `choose_text_engine` with the series consent taken as given, so a
/// page never has to read the relay's four settings fields a second time.
#[derive(Clone, Debug, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeetingTextEngineChoice {
    /// The operator's server, over the paired relay.
    Relay,
    /// The engine on this Mac.
    Local,
    /// Nothing can write it: the relay is not chosen and the engine here
    /// cannot answer. Carries that engine's state so the page can say why.
    Unavailable { engine: MeetingLocalEngineStatus },
}

#[derive(Clone, Debug)]
struct AvailabilityCache {
    checked_at: Instant,
    result: Result<Vec<LoopbackModel>, LocalEndpointError>,
}

pub(crate) struct LocalEndpointGenerator {
    endpoint: PostProcessEndpoint,
    model: String,
    model_identity: String,
    configured_context_window_tokens: Option<usize>,
    availability: Mutex<Option<AvailabilityCache>>,
}

impl LocalEndpointGenerator {
    #[cfg(test)]
    pub(crate) fn new(base_url: &str, model: &str) -> Result<Self, LocalEndpointError> {
        Self::new_with_context(base_url, model, None)
    }

    pub(crate) fn new_with_context(
        base_url: &str,
        model: &str,
        configured_context_window_tokens: Option<usize>,
    ) -> Result<Self, LocalEndpointError> {
        let endpoint = PostProcessEndpoint::meeting_local(base_url)
            .map_err(|_| LocalEndpointError::InvalidEndpoint)?;
        Ok(Self {
            endpoint: endpoint.clone(),
            model_identity: local_model_identity(&endpoint, model),
            model: model.to_string(),
            configured_context_window_tokens,
            availability: Mutex::new(None),
        })
    }

    pub(crate) fn from_settings(
        engine: &MeetingLocalEngine,
    ) -> Result<Option<Self>, LocalEndpointError> {
        match engine {
            MeetingLocalEngine::AppleIntelligence => Ok(None),
            MeetingLocalEngine::LocalEndpoint {
                base_url,
                model,
                context_window_tokens,
            } => Self::new_with_context(base_url, model, *context_window_tokens).map(Some),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_model(mut self, model: String) -> Self {
        self.model_identity = local_model_identity(&self.endpoint, &model);
        self.model = model;
        self
    }

    fn model_info(&self) -> Result<Vec<LoopbackModel>, LocalEndpointError> {
        if let Some(cache) = self
            .availability
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .filter(|cache| cache.checked_at.elapsed() < AVAILABILITY_CACHE_TTL)
        {
            return cache.result.clone();
        }

        let endpoint = self.endpoint.clone();
        let result = match run_async(async move { probe_loopback_model_info(&endpoint).await }) {
            Some(Ok(models)) => Ok(models),
            Some(Err(error)) => Err(match error {
                crate::settings::PostProcessModelDiscovery::Unreachable => {
                    LocalEndpointError::Unreachable
                }
                _ => LocalEndpointError::InvalidResponse,
            }),
            None => Err(LocalEndpointError::InvalidResponse),
        };
        *self
            .availability
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(AvailabilityCache {
            checked_at: Instant::now(),
            result: result.clone(),
        });
        result
    }

    pub(crate) fn models(&self) -> Result<Vec<String>, LocalEndpointError> {
        self.model_info()
            .map(|models| models.into_iter().map(|model| model.id).collect::<Vec<_>>())
    }

    fn configured_context_window_bytes(&self) -> Option<usize> {
        let tokens = self.configured_context_window_tokens.or_else(|| {
            self.model_info()
                .ok()?
                .into_iter()
                .find(|model| serves(&model.id, &self.model))
                .and_then(|model| model.context_window_tokens)
        })?;
        tokens.checked_mul(LOCAL_CONTEXT_BYTES_PER_TOKEN)
    }

    pub(crate) fn status(&self) -> MeetingLocalEngineStatus {
        match self.models() {
            Ok(models) => MeetingLocalEngineStatus::LocalEndpoint {
                reachable: true,
                model_count: models.len(),
                error: self.configuration_gap(&models).map(str::to_string),
            },
            Err(error) => MeetingLocalEngineStatus::LocalEndpoint {
                reachable: !matches!(error, LocalEndpointError::Unreachable),
                model_count: 0,
                error: Some(error.reason_code().to_string()),
            },
        }
    }

    /// What still stands between a reachable endpoint and a request. The
    /// reasons `is_available` refuses on, so a status never calls an endpoint
    /// ready that a generation would not ask.
    ///
    /// A selected model the catalog does not list is a gap of its own. The
    /// status used to read ready as long as *some* model was served, and the
    /// first generation then asked for a model the server had never heard of
    /// and failed with a reason that named the engine, not the setting.
    fn configuration_gap(&self, models: &[String]) -> Option<&'static str> {
        if self.model.trim().is_empty() {
            Some("model_not_selected")
        } else if models.is_empty() {
            Some("no_models")
        } else if !models.iter().any(|served| serves(served, &self.model)) {
            Some("model_not_served")
        } else if self.configured_context_window_bytes().is_none() {
            Some("context_window_not_configured")
        } else {
            None
        }
    }
}

/// Whether the catalog id `served` is the model `chosen` names. Ollama lists
/// every model with its tag and answers a name without one as that name's
/// `:latest` tag.
fn serves(served: &str, chosen: &str) -> bool {
    served == chosen || served.strip_suffix(":latest") == Some(chosen)
}

impl MeetingTextGenerator for LocalEndpointGenerator {
    fn is_available(&self) -> bool {
        self.models()
            .is_ok_and(|models| self.configuration_gap(&models).is_none())
    }

    fn model_id(&self) -> &'static str {
        LOCAL_ENGINE_MODEL_ID
    }

    fn model_version(&self) -> Cow<'static, str> {
        Cow::Owned(self.model_identity.clone())
    }

    fn max_input_bytes(&self) -> usize {
        // The OpenAI-compatible wire has no independent input ceiling. When a
        // model advertises or the user configures its context, the processing
        // seam uses context_window_bytes below instead.
        usize::MAX
    }

    fn context_window_bytes(&self) -> Option<usize> {
        // Unknown local context must refuse evidence, not remove the ceiling.
        Some(self.configured_context_window_bytes().unwrap_or(0))
    }

    fn generate(
        &self,
        system_prompt: &str,
        evidence: &str,
        max_tokens: i32,
        shape: ReplyShape,
    ) -> Result<String, MeetingTextGenerationError> {
        let endpoint = self.endpoint.clone();
        let model = self.model.clone();
        let system_prompt = system_prompt.to_string();
        let mut labels = SourceLabels::default();
        let evidence = match shape {
            ReplyShape::MeetingNotes | ReplyShape::MeetingLedger => labels.alias(evidence),
            _ => None,
        }
        .unwrap_or_else(|| evidence.to_string());
        let reply = match shape.wire_schema() {
            Some((name, schema)) => LoopbackReply::Schema(name, schema),
            None if shape.is_json() => LoopbackReply::Json,
            None => LoopbackReply::Text,
        };
        let response = run_async(async move {
            send_loopback_chat_completion(
                &endpoint,
                &model,
                &system_prompt,
                &evidence,
                max_tokens,
                MEETING_REQUEST_TIMEOUT,
                reply,
            )
            .await
        })
        .unwrap_or(Err(LoopbackChatCompletionError::Failed))
        .map_err(|error| match error {
            LoopbackChatCompletionError::Unreachable => MeetingTextGenerationError::Unreachable,
            LoopbackChatCompletionError::TimedOut => MeetingTextGenerationError::TimedOut,
            LoopbackChatCompletionError::Failed => MeetingTextGenerationError::Failed,
        })?;
        log::info!(
            "Local meeting model {}: prompt {} tokens, answer {} tokens, finished by {}",
            self.model,
            response
                .prompt_tokens
                .map_or_else(|| "unknown".to_string(), |value| value.to_string()),
            response
                .completion_tokens
                .map_or_else(|| "unknown".to_string(), |value| value.to_string()),
            response.finish_reason.as_deref().unwrap_or("unknown")
        );
        let content = response.content.ok_or(MeetingTextGenerationError::Failed)?;
        let content = strip_code_fences(&content);
        if content.is_empty() {
            Err(MeetingTextGenerationError::Failed)
        } else {
            Ok(labels.restore(content))
        }
    }
}

/// The line ids a local model reads and cites in place of the transcript's
/// uuids: `L1` for the first id a pack names, `L2` for the next.
///
/// A uuid is 34 tokens for qwen3:14b to read and as many to write, and a
/// part's notes cite dozens of lines. On one part of a test meeting the
/// notes cited by label took 970 answer tokens and 60 s, cited by uuid 2,256
/// tokens and 139 s, every citation found both times.
#[derive(Default)]
struct SourceLabels(Vec<String>);

impl SourceLabels {
    /// `pack` with every line id under its label, or `None` when it names
    /// none and goes out as it is.
    fn alias(&mut self, pack: &str) -> Option<String> {
        let mut value: Value = serde_json::from_str(pack).ok()?;
        self.alias_ids(&mut value);
        (!self.0.is_empty()).then(|| value.to_string())
    }

    fn alias_ids(&mut self, value: &mut Value) {
        match value {
            Value::Object(object) => {
                for (key, child) in object.iter_mut() {
                    match child {
                        Value::String(id)
                            if matches!(key.as_str(), "entity_id" | "segment_uuid") =>
                        {
                            let index = match self.0.iter().position(|known| *known == **id) {
                                Some(index) => index,
                                None => {
                                    self.0.push(std::mem::take(id));
                                    self.0.len() - 1
                                }
                            };
                            *id = format!("L{}", index + 1);
                        }
                        _ => self.alias_ids(child),
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|item| self.alias_ids(item)),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }

    /// `reply` with every label it cites back under its id. A label the pack
    /// never named stays as written, for the reply's reader to refuse.
    fn restore(&self, reply: String) -> String {
        if self.0.is_empty() {
            return reply;
        }
        let mut values = serde_json::Deserializer::from_str(&reply).into_iter::<Value>();
        let Some(Ok(mut value)) = values.next() else {
            return reply;
        };
        self.restore_ids(&mut value);
        format!("{value}{}", &reply[values.byte_offset()..])
    }

    fn restore_ids(&self, value: &mut Value) {
        match value {
            Value::Object(object) => {
                for (key, child) in object.iter_mut() {
                    match child {
                        Value::Array(citations) if key == "citations" => {
                            for citation in citations {
                                if let Some(id) = citation.as_str().and_then(|label| self.id(label))
                                {
                                    *citation = Value::String(id.clone());
                                }
                            }
                        }
                        _ => self.restore_ids(child),
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|item| self.restore_ids(item)),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }

    fn id(&self, label: &str) -> Option<&String> {
        let number = label.strip_prefix('L')?.parse::<usize>().ok()?;
        self.0.get(number.checked_sub(1)?)
    }
}

fn strip_code_fences(content: &str) -> String {
    let trimmed = content.trim();
    let without_opening = if let Some(rest) = trimmed.strip_prefix("```") {
        rest.find('\n').map_or(rest, |newline| &rest[newline + 1..])
    } else {
        trimmed
    };
    without_opening
        .strip_suffix("```")
        .unwrap_or(without_opening)
        .trim()
        .to_string()
}

fn local_model_identity(endpoint: &PostProcessEndpoint, model: &str) -> String {
    format!("{}|{}", endpoint.base_url(), model)
}

fn run_async<T: Send + 'static>(future: impl Future<Output = T> + Send + 'static) -> Option<T> {
    thread::spawn(move || tauri::async_runtime::block_on(future))
        .join()
        .ok()
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;
    use std::sync::mpsc::Receiver;

    pub(crate) fn expire_availability(generator: &LocalEndpointGenerator) {
        generator
            .availability
            .lock()
            .expect("availability cache")
            .as_mut()
            .expect("discovered context")
            .checked_at -= AVAILABILITY_CACHE_TTL;
    }

    pub(crate) struct FixtureServer {
        pub(crate) base_url: String,
        pub(crate) requests: Receiver<String>,
        pub(crate) connections: Arc<AtomicUsize>,
        pub(crate) handle: thread::JoinHandle<()>,
    }

    pub(crate) fn fixture_server(responses: Vec<String>) -> FixtureServer {
        fixture_server_with_status(responses.into_iter().map(|body| (200, body)).collect())
    }

    pub(crate) fn fixture_server_with_status(responses: Vec<(u16, String)>) -> FixtureServer {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let (sender, requests) = mpsc::channel();
        let connections = Arc::new(AtomicUsize::new(0));
        let connection_count = Arc::clone(&connections);
        let handle = thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("fixture connection");
                connection_count.fetch_add(1, Ordering::Relaxed);
                let request = read_http_request(&mut stream);
                sender.send(request).expect("fixture request receiver");
                let status_line = if status == 200 {
                    "200 OK".to_string()
                } else {
                    format!("{status} Test")
                };
                let response = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("fixture response");
            }
        });
        FixtureServer {
            base_url: format!("http://{address}/v1"),
            requests,
            connections,
            handle,
        }
    }

    pub(crate) fn read_http_request(stream: &mut impl Read) -> String {
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 4096];
        let header_end = loop {
            let count = stream.read(&mut chunk).expect("fixture request read");
            assert!(count > 0, "fixture request ended before headers");
            bytes.extend_from_slice(&chunk[..count]);
            if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let headers = String::from_utf8_lossy(&bytes[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        while bytes.len() < header_end + content_length {
            let count = stream.read(&mut chunk).expect("fixture body read");
            assert!(count > 0, "fixture body ended early");
            bytes.extend_from_slice(&chunk[..count]);
        }
        String::from_utf8(bytes).expect("fixture request utf8")
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct RequestBody {
        model: String,
        max_tokens: i32,
        reasoning_effort: String,
        response_format: Option<ResponseFormat>,
        messages: Vec<RequestMessage>,
    }

    #[derive(Deserialize)]
    struct ResponseFormat {
        #[serde(rename = "type")]
        kind: String,
        json_schema: Option<WireSchema>,
    }

    #[derive(Deserialize)]
    struct WireSchema {
        name: String,
        schema: Value,
    }

    #[derive(Deserialize)]
    struct RequestMessage {
        role: String,
        content: String,
    }

    fn request_body(request: &str) -> RequestBody {
        serde_json::from_str(request.split_once("\r\n\r\n").expect("request body").1)
            .expect("request json")
    }

    #[test]
    fn sends_required_json_request_and_strips_fences() {
        for (shape, kind, schema) in [
            (ReplyShape::Json, "json_object", None),
            (
                ReplyShape::MeetingNotes,
                "json_schema",
                Some("meeting_notes"),
            ),
            (
                ReplyShape::MeetingLedger,
                "json_schema",
                Some("meeting_ledger"),
            ),
        ] {
            let fixture = fixture_server(vec![
                r#"{"choices":[{"message":{"content":"```json\n{\"headline\":\"done\"}\n```"},"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":4}}"#.to_string(),
            ]);
            let generator =
                LocalEndpointGenerator::new(&fixture.base_url, "fixture-model").expect("generator");
            let answer = generator
                .generate("system", "evidence", 42, shape)
                .expect("generation");
            assert_eq!(answer, r#"{"headline":"done"}"#);
            let body = request_body(&fixture.requests.recv().expect("request"));
            assert_eq!(body.model, "fixture-model");
            assert_eq!(body.max_tokens, 42);
            assert_eq!(body.reasoning_effort, "none");
            let format = body.response_format.expect("a response format");
            assert_eq!(format.kind, kind);
            assert_eq!(
                format.json_schema.as_ref().map(|wire| wire.name.as_str()),
                schema
            );
            if let Some(wire) = format
                .json_schema
                .filter(|wire| wire.name == "meeting_ledger")
            {
                assert_eq!(
                    wire.schema["definitions"]["RawLedgerReceipt"]["properties"]["quote"]["type"],
                    "string"
                );
            }
            assert_eq!(body.messages[0].role, "system");
            assert_eq!(body.messages[1].content, "evidence");
            fixture.handle.join().expect("fixture thread");
        }
    }

    #[test]
    fn omits_json_response_format_for_prose() {
        let fixture = fixture_server(vec![
            r#"{"choices":[{"message":{"content":"plain answer"}}]}"#.to_string(),
        ]);
        let generator =
            LocalEndpointGenerator::new(&fixture.base_url, "fixture-model").expect("generator");
        assert_eq!(
            generator.generate("system", "evidence", 20, ReplyShape::Prose),
            Ok("plain answer".to_string())
        );
        let body = request_body(&fixture.requests.recv().expect("request"));
        assert!(body.response_format.is_none());
        fixture.handle.join().expect("fixture thread");
    }

    #[test]
    fn empty_content_is_failed() {
        let fixture = fixture_server(vec![
            r#"{"choices":[{"message":{"content":"   "}}]}"#.to_string()
        ]);
        let generator =
            LocalEndpointGenerator::new(&fixture.base_url, "fixture-model").expect("generator");
        assert_eq!(
            generator.generate("system", "evidence", 20, ReplyShape::Prose),
            Err(MeetingTextGenerationError::Failed)
        );
        fixture.handle.join().expect("fixture thread");
    }

    #[test]
    fn unreachable_generation_is_unreachable() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let address = listener.local_addr().expect("address");
        drop(listener);
        let generator =
            LocalEndpointGenerator::new(&format!("http://{address}/v1"), "fixture-model")
                .expect("generator");
        assert_eq!(
            generator.generate("system", "evidence", 20, ReplyShape::Prose),
            Err(MeetingTextGenerationError::Unreachable)
        );
    }

    #[test]
    fn remote_endpoint_is_rejected() {
        assert!(matches!(
            LocalEndpointGenerator::new("https://example.com/v1", "model"),
            Err(LocalEndpointError::InvalidEndpoint)
        ));
    }

    #[test]
    fn availability_is_false_without_server_and_cached_with_fixture() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let address = listener.local_addr().expect("address");
        drop(listener);
        let absent = LocalEndpointGenerator::new(&format!("http://{address}/v1"), "model")
            .expect("generator");
        assert!(!absent.is_available());

        let fixture = fixture_server(vec![r#"{"data":[{"id":"model"}]}"#.to_string()]);
        let present =
            LocalEndpointGenerator::new_with_context(&fixture.base_url, "model", Some(8192))
                .expect("generator");
        assert!(present.is_available());
        assert!(present.is_available());
        assert_eq!(fixture.connections.load(Ordering::Relaxed), 1);
        fixture.handle.join().expect("fixture thread");
    }

    #[test]
    fn a_model_chosen_without_its_latest_tag_is_served() {
        let fixture = fixture_server(vec![
            r#"{"object":"list","data":[{"id":"sona-notes:latest","object":"model","context_length":16384}]}"#
                .to_string(),
        ]);
        let generator =
            LocalEndpointGenerator::new_with_context(&fixture.base_url, "sona-notes", None)
                .expect("generator");
        assert!(generator.is_available());
        assert_eq!(
            generator.context_window_bytes(),
            Some(16_384 * LOCAL_CONTEXT_BYTES_PER_TOKEN)
        );
        fixture.handle.join().expect("fixture thread");
    }

    #[test]
    fn a_notes_part_is_read_and_cited_by_line_labels() {
        let first = "00000000-0000-0000-0000-000000000001";
        let second = "00000000-0000-0000-0000-000000000002";
        let reply = serde_json::json!({
            "summary": [{"text": "Shipped.", "citations": ["L2", "L1", "L9"]}]
        });
        let fixture = fixture_server(vec![serde_json::json!({
            "choices": [{"message": {"content": reply.to_string()}, "finish_reason": "stop"}]
        })
        .to_string()]);
        let generator =
            LocalEndpointGenerator::new(&fixture.base_url, "fixture-model").expect("generator");
        let pack = serde_json::json!({"transcript": [
            {"entity_id": first, "text": "We ship Monday."},
            {"entity_id": second, "text": "Agreed."}
        ]});
        let answer = generator
            .generate("system", &pack.to_string(), 42, ReplyShape::MeetingNotes)
            .expect("generation");
        let body = request_body(&fixture.requests.recv().expect("request"));
        let sent: Value = serde_json::from_str(&body.messages[1].content).expect("sent part");
        assert_eq!(sent["transcript"][0]["entity_id"], "L1");
        assert_eq!(sent["transcript"][1]["entity_id"], "L2");
        let answer: Value = serde_json::from_str(&answer).expect("restored reply");
        assert_eq!(
            answer["summary"][0]["citations"],
            serde_json::json!([second, first, "L9"])
        );
        fixture.handle.join().expect("fixture thread");
    }

    #[test]
    fn reachable_invalid_catalog_is_reported_not_skipped() {
        let fixture = fixture_server_with_status(vec![(503, r#"{"error":"busy"}"#.to_string())]);
        let generator =
            LocalEndpointGenerator::new_with_context(&fixture.base_url, "model", Some(8192))
                .expect("generator");

        assert!(!generator.is_available());
        assert_eq!(
            generator.status(),
            MeetingLocalEngineStatus::LocalEndpoint {
                reachable: true,
                model_count: 0,
                error: Some("invalid_response".to_string()),
            }
        );
        assert_eq!(fixture.connections.load(Ordering::Relaxed), 1);
        fixture.handle.join().expect("fixture thread");
    }

    /// A start page shows the status as the reason nothing will write the
    /// next meeting's notes, so an endpoint `is_available` refuses must
    /// never read as healthy. One test per gap, each reporting on its own.
    fn gap_is_reported(models: &[&str], model: &str, context: Option<usize>, code: &str) {
        let catalog = serde_json::json!({
            "data": models.iter().map(|id| serde_json::json!({ "id": id })).collect::<Vec<_>>()
        });
        let fixture = fixture_server(vec![catalog.to_string()]);
        let generator = LocalEndpointGenerator::new_with_context(&fixture.base_url, model, context)
            .expect("generator");
        assert!(!generator.is_available());
        assert_eq!(
            generator.status(),
            MeetingLocalEngineStatus::LocalEndpoint {
                reachable: true,
                model_count: models.len(),
                error: Some(code.to_string()),
            }
        );
        fixture.handle.join().expect("fixture thread");
    }

    #[test]
    fn unselected_model_is_reported_as_the_gap() {
        gap_is_reported(&["model"], "", Some(8192), "model_not_selected");
    }

    #[test]
    fn empty_catalog_is_reported_as_the_gap() {
        gap_is_reported(&[], "model", Some(8192), "no_models");
    }

    #[test]
    fn unstated_context_window_is_reported_as_the_gap() {
        gap_is_reported(&["model"], "model", None, "context_window_not_configured");
    }

    #[test]
    fn a_selected_model_the_catalog_lacks_is_reported_as_the_gap() {
        gap_is_reported(&["other-model"], "model", Some(8192), "model_not_served");
    }
}
