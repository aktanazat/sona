use super::processing::{MeetingTextGenerationError, MeetingTextGenerator, ReplyShape};
use crate::llm_client::{
    probe_loopback_model_info, send_loopback_chat_completion, LoopbackChatCompletionError,
    LoopbackModel,
};
use crate::settings::{MeetingLocalEngine, PostProcessEndpoint, PostProcessProvider};
use serde::Serialize;
use specta::Type;
use std::borrow::Cow;
use std::future::Future;
#[cfg(test)]
use std::io::{Read, Write};
#[cfg(test)]
use std::net::{TcpListener, TcpStream};
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
const LOCAL_CONTEXT_BYTES_PER_TOKEN: usize = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LocalEndpointError {
    InvalidEndpoint,
    Unreachable,
    InvalidResponse,
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

#[derive(Clone, Debug, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeetingLocalEngineStatus {
    AppleIntelligence {
        available: bool,
    },
    LocalEndpoint {
        reachable: bool,
        model_count: usize,
        error: Option<String>,
    },
}

#[derive(Clone, Debug)]
struct AvailabilityCache {
    checked_at: Instant,
    result: Result<Vec<LoopbackModel>, LocalEndpointError>,
}

pub(crate) struct LocalEndpointGenerator {
    endpoint: PostProcessEndpoint,
    model: String,
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
        let provider = PostProcessProvider {
            id: "custom".to_string(),
            label: "Custom".to_string(),
            base_url: base_url.trim().to_string(),
            allow_base_url_edit: true,
            supports_structured_output: false,
        };
        let endpoint = provider
            .endpoint()
            .map_err(|_| LocalEndpointError::InvalidEndpoint)?;
        if endpoint.is_remote() || !endpoint.base_url().trim_end_matches('/').ends_with("/v1") {
            return Err(LocalEndpointError::InvalidEndpoint);
        }
        Ok(Self {
            endpoint,
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
                .find(|model| model.id == self.model)
                .and_then(|model| model.context_window_tokens)
        })?;
        tokens.checked_mul(LOCAL_CONTEXT_BYTES_PER_TOKEN)
    }

    pub(crate) fn status(&self) -> MeetingLocalEngineStatus {
        match self.models() {
            Ok(models) => MeetingLocalEngineStatus::LocalEndpoint {
                reachable: true,
                model_count: models.len(),
                error: (!self.model.trim().is_empty()
                    && self.configured_context_window_bytes().is_none())
                .then(|| "context window is not configured".to_string()),
            },
            Err(error) => MeetingLocalEngineStatus::LocalEndpoint {
                reachable: !matches!(error, LocalEndpointError::Unreachable),
                model_count: 0,
                error: Some(error.to_string()),
            },
        }
    }
}

impl MeetingTextGenerator for LocalEndpointGenerator {
    fn is_available(&self) -> bool {
        !self.model.trim().is_empty()
            && self.models().is_ok_and(|models| {
                !models.is_empty() && self.configured_context_window_bytes().is_some()
            })
    }

    fn model_id(&self) -> &'static str {
        LOCAL_ENGINE_MODEL_ID
    }

    fn model_version(&self) -> Cow<'static, str> {
        Cow::Owned(self.model.clone())
    }

    fn max_input_bytes(&self) -> usize {
        // The OpenAI-compatible wire has no independent input ceiling. When a
        // model advertises or the user configures its context, the processing
        // seam uses context_window_bytes below instead.
        usize::MAX
    }

    fn context_window_bytes(&self) -> Option<usize> {
        self.configured_context_window_bytes()
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
        let evidence = evidence.to_string();
        let json_response = shape == ReplyShape::Json;
        let response = run_async(async move {
            send_loopback_chat_completion(
                &endpoint,
                &model,
                &system_prompt,
                &evidence,
                max_tokens,
                MEETING_REQUEST_TIMEOUT,
                json_response,
            )
            .await
        })
        .unwrap_or(Err(LoopbackChatCompletionError::Failed))
        .map_err(|error| match error {
            LoopbackChatCompletionError::Unreachable => MeetingTextGenerationError::Unreachable,
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
            Ok(content)
        }
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

fn run_async<T: Send + 'static>(future: impl Future<Output = T> + Send + 'static) -> Option<T> {
    thread::spawn(move || tauri::async_runtime::block_on(future))
        .join()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::sync::mpsc::Receiver;

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
    }

    #[derive(Deserialize)]
    struct RequestMessage {
        role: String,
        content: String,
    }

    struct FixtureServer {
        base_url: String,
        requests: Receiver<String>,
        connections: Arc<AtomicUsize>,
        handle: thread::JoinHandle<()>,
    }

    fn fixture_server(responses: Vec<String>) -> FixtureServer {
        fixture_server_with_status(responses.into_iter().map(|body| (200, body)).collect())
    }

    fn fixture_server_with_status(responses: Vec<(u16, String)>) -> FixtureServer {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let (sender, requests) = mpsc::channel();
        let connections = Arc::new(AtomicUsize::new(0));
        let connection_count = Arc::clone(&connections);
        let handle = thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("fixture connection");
                connection_count.fetch_add(1, Ordering::Relaxed);
                let request = read_request(&mut stream);
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

    fn read_request(stream: &mut TcpStream) -> String {
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
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .and_then(|length| length.trim().parse::<usize>().ok())
            .unwrap_or(0);
        while bytes.len() < header_end + content_length {
            let count = stream.read(&mut chunk).expect("fixture body read");
            assert!(count > 0, "fixture body ended early");
            bytes.extend_from_slice(&chunk[..count]);
        }
        String::from_utf8(bytes).expect("fixture request utf8")
    }
    fn request_body(request: &str) -> RequestBody {
        serde_json::from_str(request.split_once("\r\n\r\n").expect("request body").1)
            .expect("request json")
    }

    #[test]
    fn sends_required_json_request_and_strips_fences() {
        let fixture = fixture_server(vec![
            r#"{"choices":[{"message":{"content":"```json\n{\"headline\":\"done\"}\n```"},"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":4}}"#.to_string(),
        ]);
        let generator =
            LocalEndpointGenerator::new(&fixture.base_url, "fixture-model").expect("generator");
        let answer = generator
            .generate("system", "evidence", 42, ReplyShape::Json)
            .expect("generation");
        assert_eq!(answer, r#"{"headline":"done"}"#);
        let body = request_body(&fixture.requests.recv().expect("request"));
        assert_eq!(body.model, "fixture-model");
        assert_eq!(body.max_tokens, 42);
        assert_eq!(body.reasoning_effort, "none");
        assert_eq!(
            body.response_format
                .as_ref()
                .map(|format| format.kind.as_str()),
            Some("json_object")
        );
        assert_eq!(body.messages[0].role, "system");
        assert_eq!(body.messages[1].content, "evidence");
        fixture.handle.join().expect("fixture thread");
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
                error: Some("endpoint returned an invalid model list".to_string()),
            }
        );
        assert_eq!(fixture.connections.load(Ordering::Relaxed), 1);
        fixture.handle.join().expect("fixture thread");
    }
}
