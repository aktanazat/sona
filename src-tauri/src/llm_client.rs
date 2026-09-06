use crate::secrets::SecretValue;
use crate::settings::{
    PostProcessCatalogSource, PostProcessEndpoint, PostProcessExecutionProtocol,
    PostProcessModelDiscovery, PostProcessModelOption, PostProcessModelProvenance,
    PostProcessProvider,
};
use futures_util::StreamExt;
use log::{debug, error, info};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use reqwest::redirect::Policy;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use zeroize::Zeroizing;

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
#[serde(transparent)]
pub(crate) struct StructuredOutputSchema(pub(crate) serde_json::Value);

#[derive(Debug, Serialize)]
struct JsonSchema {
    name: String,
    strict: bool,
    schema: StructuredOutputSchema,
}

#[derive(Debug, Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    format_type: String,
    json_schema: JsonSchema,
}

#[derive(Debug, Serialize, Clone, Default, PartialEq)]
struct ReasoningConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exclude: Option<bool>,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
struct ThinkingParams {
    #[serde(rename = "type")]
    kind: &'static str,
}

/// Request fields used to ask an endpoint to skip reasoning/thinking.
/// Providers disagree on the field name and accepted values, so at most one of
/// these is set per request (see `reasoning_disable_params`).
#[derive(Debug, Serialize, Clone, Default, PartialEq)]
struct ReasoningParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ReasoningConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingParams>,
}

impl ReasoningParams {
    fn is_empty(&self) -> bool {
        self.reasoning_effort.is_none() && self.reasoning.is_none() && self.thinking.is_none()
    }
}

/// Pick the reasoning-disable request fields an endpoint understands.
/// Unknown endpoints get the common OpenAI-style field; if they reject it,
/// the request is retried without it (see `send_chat_completion_with_schema`).
fn reasoning_disable_params(
    provider: &PostProcessProvider,
    endpoint: &PostProcessEndpoint,
) -> ReasoningParams {
    let base_url = endpoint.base_url().to_lowercase();
    if base_url.contains("api.deepseek.com") {
        // DeepSeek rejects reasoning_effort "none" and uses its own field:
        // https://api-docs.deepseek.com/guides/thinking_mode
        ReasoningParams {
            thinking: Some(ThinkingParams { kind: "disabled" }),
            ..Default::default()
        }
    } else if provider.id == "openrouter" {
        // OpenRouter nested object; exclude:true also keeps reasoning text out
        // of the response so it can't pollute structured-output JSON parsing
        ReasoningParams {
            reasoning: Some(ReasoningConfig {
                effort: Some("none".to_string()),
                exclude: Some(true),
            }),
            ..Default::default()
        }
    } else {
        ReasoningParams {
            reasoning_effort: Some("none".to_string()),
            ..Default::default()
        }
    }
}

/// Endpoints (base_url|model) that rejected the reasoning-disable fields with a
/// 4xx. Remembered for the lifetime of the process so every dictation after the
/// first skips the doomed attempt and goes straight to a plain request.
fn reasoning_rejections() -> &'static Mutex<HashSet<String>> {
    static REJECTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    REJECTED.get_or_init(|| Mutex::new(HashSet::new()))
}

fn endpoint_key(endpoint: &PostProcessEndpoint, model: &str) -> String {
    format!("{}|{}", endpoint.base_url(), model)
}

fn is_known_rejected(key: &str) -> bool {
    reasoning_rejections()
        .lock()
        .map(|set| set.contains(key))
        .unwrap_or(false)
}

fn remember_rejection(key: String) {
    if let Ok(mut set) = reasoning_rejections().lock() {
        set.insert(key);
    }
}

#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
    #[serde(flatten)]
    reasoning: ReasoningParams,
}
pub(crate) struct ChatCompletionInput<'a> {
    pub provider: &'a PostProcessProvider,
    pub endpoint: &'a PostProcessEndpoint,
    pub secret: Option<&'a SecretValue>,
    pub model: &'a str,
    pub user_content: String,
    pub system_prompt: Option<String>,
    pub json_schema: Option<StructuredOutputSchema>,
    pub disable_reasoning: bool,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessageResponse,
}

#[derive(Debug, Deserialize)]
struct ChatMessageResponse {
    content: Option<String>,
}

#[derive(Debug, Serialize)]
struct AnthropicMessageRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct AnthropicMessageResponse {
    content: Vec<AnthropicContentBlock>,
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

/// Build headers for API requests based on provider type.
fn build_headers(
    provider: &PostProcessProvider,
    secret: Option<&SecretValue>,
) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();

    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(USER_AGENT, HeaderValue::from_static("Sona/1.0"));
    headers.insert("X-Title", HeaderValue::from_static("Sona"));

    if let Some(secret) = secret {
        if provider.id == "anthropic" {
            headers.insert(
                "x-api-key",
                HeaderValue::from_str(secret.expose())
                    .map_err(|_| "Invalid API key header value".to_string())?,
            );
            headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        } else {
            let bearer = Zeroizing::new(format!("Bearer {}", secret.expose()));
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(bearer.as_str())
                    .map_err(|_| "Invalid authorization header value".to_string())?,
            );
        }
    }

    Ok(headers)
}

/// One bound on every post-processing request. Without it a dictation waits
/// indefinitely on an endpoint that accepts the connection and then never
/// answers, which reqwest does not report on its own: its default is no
/// timeout at all.
///
/// Twenty seconds is chosen for the warm case and deliberately sacrifices the
/// cold one — it does not protect a model that is still loading its weights,
/// because that load does not fit in any bound a dictation can afford. A warm
/// local answer lands in well under a second; a cold 7.7 GB `gemma4:12b-mlx`
/// measured over three minutes on a loaded machine. So the first dictation
/// after a cold start does not wait for the model: it skips post-processing and
/// delivers the raw transcript, with only a log line saying so — nothing in the
/// UI reports the dropped rewrite. That is the intended trade, since unpolished
/// words beat words that arrive minutes late, and `post_process_transcription`
/// already reads the failure as "no rewrite". Raising the bound to cover a cold
/// load would buy nothing except making that first dictation hang for it.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Create an HTTP client with provider-specific headers. Redirects are disabled
/// so an Authorization header cannot reach a destination outside the frozen
/// endpoint.
fn create_client(
    provider: &PostProcessProvider,
    secret: Option<&SecretValue>,
) -> Result<reqwest::Client, String> {
    let headers = build_headers(provider, secret)?;
    reqwest::Client::builder()
        .default_headers(headers)
        .redirect(Policy::none())
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| report_reqwest_error("Failed to build HTTP client", &e))
}

fn reqwest_error_kinds(error: &reqwest::Error) -> String {
    let mut kinds = Vec::new();

    if error.is_builder() {
        kinds.push("builder");
    }
    if error.is_connect() {
        kinds.push("connect");
    }
    if error.is_request() {
        kinds.push("request");
    }
    if error.is_redirect() {
        kinds.push("redirect");
    }
    if error.is_timeout() {
        kinds.push("timeout");
    }
    if error.is_status() {
        kinds.push("status");
    }
    if error.is_body() {
        kinds.push("body");
    }
    if error.is_decode() {
        kinds.push("decode");
    }
    if error.is_upgrade() {
        kinds.push("upgrade");
    }

    if kinds.is_empty() {
        "unknown".to_string()
    } else {
        kinds.join(", ")
    }
}

fn report_reqwest_error(context: &str, error: &reqwest::Error) -> String {
    let details = format!("{context} (kind: {})", reqwest_error_kinds(error));
    error!("{details}");
    details
}

fn endpoint_matches_provider(
    provider: &PostProcessProvider,
    endpoint: &PostProcessEndpoint,
) -> bool {
    provider
        .endpoint()
        .is_ok_and(|current| current == *endpoint)
}

/// Send a chat completion request to an OpenAI-compatible API
/// Returns Ok(Some(content)) on success, Ok(None) if response has no content,
/// or Err on actual errors (HTTP, parsing, etc.)
pub async fn send_chat_completion(
    provider: &PostProcessProvider,
    endpoint: &PostProcessEndpoint,
    secret: Option<&SecretValue>,
    model: &str,
    prompt: String,
    disable_reasoning: bool,
) -> Result<Option<String>, String> {
    send_chat_completion_with_schema(ChatCompletionInput {
        provider,
        endpoint,
        secret,
        model,
        user_content: prompt,
        system_prompt: None,
        json_schema: None,
        disable_reasoning,
    })
    .await
}

/// Send a post-processing request through the protocol owned by its provider.
pub(crate) async fn send_chat_completion_with_schema(
    input: ChatCompletionInput<'_>,
) -> Result<Option<String>, String> {
    match input.provider.execution_protocol() {
        PostProcessExecutionProtocol::OpenAiChatCompletions => {
            send_openai_chat_completion_with_schema(input).await
        }
        PostProcessExecutionProtocol::AnthropicMessages => send_anthropic_message(input).await,
    }
}

/// OpenAI-compatible execution with the existing one-time retry for unsupported
/// reasoning controls.
async fn send_openai_chat_completion_with_schema(
    input: ChatCompletionInput<'_>,
) -> Result<Option<String>, String> {
    let ChatCompletionInput {
        provider,
        endpoint,
        secret,
        model,
        user_content,
        system_prompt,
        json_schema,
        disable_reasoning,
    } = input;
    if !endpoint_matches_provider(provider, endpoint) {
        return Err("Post-processing destination changed".to_string());
    }
    let url = endpoint.request_url("chat/completions");

    debug!("Sending OpenAI-compatible chat completion request");

    let client = create_client(provider, secret)?;
    let mut messages = Vec::new();
    if let Some(system) = system_prompt {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: system,
        });
    }
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: user_content,
    });

    let response_format = json_schema.map(|schema| ResponseFormat {
        format_type: "json_schema".to_string(),
        json_schema: JsonSchema {
            name: "transcription_output".to_string(),
            strict: true,
            schema,
        },
    });

    let key = endpoint_key(endpoint, model);
    let reasoning = if disable_reasoning && !is_known_rejected(&key) {
        reasoning_disable_params(provider, endpoint)
    } else {
        ReasoningParams::default()
    };

    let mut request_body = ChatCompletionRequest {
        model: model.to_string(),
        messages,
        stream: false,
        response_format,
        reasoning,
    };

    let mut response = client
        .post(&url)
        .json(&request_body)
        .send()
        .await
        .map_err(|error| report_reqwest_error("HTTP request failed", &error))?;
    let mut status = response.status();
    debug!(
        "Chat completion response received with status {} over {:?}",
        status,
        response.version()
    );

    if !status.is_success()
        && matches!(status.as_u16(), 400 | 422)
        && !request_body.reasoning.is_empty()
    {
        // Provider bodies are not useful for this retry and may contain
        // submitted text, so never materialize them.
        drop(response);
        info!(
            "Endpoint rejected reasoning-disable fields with status {}; retrying without them",
            status
        );

        request_body.reasoning = ReasoningParams::default();
        response = client
            .post(&url)
            .json(&request_body)
            .send()
            .await
            .map_err(|error| report_reqwest_error("HTTP retry failed", &error))?;
        status = response.status();
        debug!(
            "Chat completion retry response received with status {} over {:?}",
            status,
            response.version()
        );

        if status.is_success() {
            info!(
                "Retry without reasoning fields succeeded; the frozen destination will skip them"
            );
            remember_rejection(key);
        }
    }

    if !status.is_success() {
        return Err(format!("API request failed with status {status}"));
    }

    let completion: ChatCompletionResponse = response
        .json()
        .await
        .map_err(|error| report_reqwest_error("Failed to parse API response", &error))?;

    Ok(completion
        .choices
        .first()
        .and_then(|choice| choice.message.content.clone()))
}

const ANTHROPIC_MAX_OUTPUT_TOKENS: u32 = 4096;

/// Anthropic's Messages API has a different path, request body, and response
/// envelope from the OpenAI-compatible providers.
async fn send_anthropic_message(input: ChatCompletionInput<'_>) -> Result<Option<String>, String> {
    let ChatCompletionInput {
        provider,
        endpoint,
        secret,
        model,
        user_content,
        system_prompt,
        json_schema: _,
        disable_reasoning: _,
    } = input;
    if !endpoint_matches_provider(provider, endpoint) {
        return Err("Post-processing destination changed".to_string());
    }

    let client = create_client(provider, secret)?;
    let request = AnthropicMessageRequest {
        model: model.to_string(),
        max_tokens: ANTHROPIC_MAX_OUTPUT_TOKENS,
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: user_content,
        }],
        system: system_prompt,
        stream: false,
    };
    let response = client
        .post(endpoint.request_url("messages"))
        .json(&request)
        .send()
        .await
        .map_err(|error| report_reqwest_error("Anthropic Messages request failed", &error))?;
    let status = response.status();
    debug!(
        "Anthropic Messages response received with status {} over {:?}",
        status,
        response.version()
    );
    if !status.is_success() {
        return Err(format!("API request failed with status {status}"));
    }

    let response: AnthropicMessageResponse = response
        .json()
        .await
        .map_err(|error| report_reqwest_error("Failed to parse Anthropic response", &error))?;
    // A reply clipped at the ceiling is a partial rewrite, and
    // `post_process_transcription` reads an error as "no rewrite" and delivers the
    // raw transcript. Unpolished words beat a dictation missing its tail.
    if response.stop_reason.as_deref() == Some("max_tokens") {
        return Err("Anthropic stopped the reply at the output-token ceiling".to_string());
    }
    Ok(response
        .content
        .into_iter()
        .find(|block| block.kind == "text")
        .and_then(|block| block.text))
}

/// Cap decoded provider catalog bytes before JSON parsing. Reqwest's stream
/// applies transparent gzip, brotli, and deflate decoding before this limit.
const MAX_CATALOG_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_CATALOG_MODEL_ID_BYTES: usize = 200;
const MAX_CATALOG_MODELS: usize = 200;
const MAX_ANTHROPIC_CATALOG_PAGES: usize = 20;

#[derive(Debug, Deserialize)]
struct OpenAiCatalogResponse {
    data: Vec<OpenAiCatalogModel>,
}

#[derive(Debug, Deserialize)]
struct OpenAiCatalogModel {
    id: String,
}

#[derive(Debug, Deserialize)]
struct OpenRouterCatalogResponse {
    data: Vec<OpenRouterCatalogModel>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterCatalogModel {
    id: String,
    architecture: Option<OpenRouterArchitecture>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterArchitecture {
    #[serde(default)]
    input_modalities: Vec<String>,
    #[serde(default)]
    output_modalities: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicCatalogResponse {
    data: Vec<AnthropicCatalogModel>,
    has_more: bool,
    last_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicCatalogModel {
    id: String,
}

/// Discover only through a provider-owned closed catalog strategy. The caller
/// supplies a frozen, validated endpoint and an optional credential; neither
/// is placed in the result or in an error string.
pub(crate) async fn discover_models(
    provider: &PostProcessProvider,
    endpoint: &PostProcessEndpoint,
    secret: Option<&SecretValue>,
) -> Result<Vec<PostProcessModelOption>, PostProcessModelDiscovery> {
    if !endpoint_matches_provider(provider, endpoint) {
        return Err(PostProcessModelDiscovery::InvalidDestination);
    }

    match provider.catalog_source() {
        PostProcessCatalogSource::OpenAi => {
            discover_openai_compatible_catalog(provider, endpoint, secret, openai_model_is_eligible)
                .await
        }
        PostProcessCatalogSource::OpenRouter => {
            discover_openrouter_catalog(provider, endpoint, secret).await
        }
        PostProcessCatalogSource::Anthropic => {
            discover_anthropic_catalog(provider, endpoint, secret).await
        }
        PostProcessCatalogSource::Groq => {
            discover_openai_compatible_catalog(provider, endpoint, secret, groq_model_is_eligible)
                .await
        }
        PostProcessCatalogSource::Cerebras => {
            discover_openai_compatible_catalog(
                provider,
                endpoint,
                secret,
                cerebras_model_is_eligible,
            )
            .await
        }
        PostProcessCatalogSource::CustomOpenAiCompatible => {
            discover_openai_compatible_catalog(provider, endpoint, secret, |_| true).await
        }
        PostProcessCatalogSource::Unsupported => Err(PostProcessModelDiscovery::Unsupported),
    }
}

async fn discover_openai_compatible_catalog(
    provider: &PostProcessProvider,
    endpoint: &PostProcessEndpoint,
    secret: Option<&SecretValue>,
    is_eligible: fn(&str) -> bool,
) -> Result<Vec<PostProcessModelOption>, PostProcessModelDiscovery> {
    debug!("Fetching OpenAI-compatible post-processing model catalog");
    let client = create_catalog_client(provider, secret)?;
    let response: OpenAiCatalogResponse =
        get_catalog_json(client.get(endpoint.request_url("models"))).await?;
    let mut models = Vec::new();
    let mut seen = HashSet::new();
    for entry in response.data {
        if is_eligible(&entry.id) && catalog_id_is_safe(&entry.id) {
            append_provider_reported_model(&mut models, &mut seen, &entry.id);
        }
    }
    Ok(models)
}

async fn discover_openrouter_catalog(
    provider: &PostProcessProvider,
    endpoint: &PostProcessEndpoint,
    secret: Option<&SecretValue>,
) -> Result<Vec<PostProcessModelOption>, PostProcessModelDiscovery> {
    debug!("Fetching OpenRouter post-processing model catalog");
    let client = create_catalog_client(provider, secret)?;
    let response: OpenRouterCatalogResponse =
        get_catalog_json(client.get(endpoint.request_url("models"))).await?;
    let mut models = Vec::new();
    let mut seen = HashSet::new();
    for entry in response.data {
        if entry
            .architecture
            .as_ref()
            .is_some_and(openrouter_architecture_supports_text_io)
            && catalog_id_is_safe(&entry.id)
        {
            append_provider_reported_model(&mut models, &mut seen, &entry.id);
        }
    }
    Ok(models)
}

async fn discover_anthropic_catalog(
    provider: &PostProcessProvider,
    endpoint: &PostProcessEndpoint,
    secret: Option<&SecretValue>,
) -> Result<Vec<PostProcessModelOption>, PostProcessModelDiscovery> {
    debug!("Fetching Anthropic post-processing model catalog");
    let client = create_catalog_client(provider, secret)?;
    let url = endpoint.request_url("models");
    let mut models = Vec::new();
    let mut seen_models = HashSet::new();
    let mut seen_cursors = HashSet::new();
    let mut after_id = None;

    for _ in 0..MAX_ANTHROPIC_CATALOG_PAGES {
        let request = match after_id.as_deref() {
            Some(cursor) => client.get(&url).query(&[("after_id", cursor)]),
            None => client.get(&url),
        };
        let response: AnthropicCatalogResponse = get_catalog_json(request).await?;
        for entry in response.data {
            if anthropic_model_is_eligible(&entry.id) && catalog_id_is_safe(&entry.id) {
                append_provider_reported_model(&mut models, &mut seen_models, &entry.id);
            }
        }
        if !response.has_more || models.len() >= MAX_CATALOG_MODELS {
            return Ok(models);
        }

        let Some(last_id) = response.last_id else {
            return Err(PostProcessModelDiscovery::InvalidResponse);
        };
        // The cursor is handed back to Anthropic on the next request, so an
        // unusable or repeated one ends the walk instead of being skipped.
        if !catalog_id_is_safe(&last_id) || !seen_cursors.insert(last_id.clone()) {
            return Err(PostProcessModelDiscovery::InvalidResponse);
        }
        after_id = Some(last_id);
    }

    Err(PostProcessModelDiscovery::InvalidResponse)
}

fn create_catalog_client(
    provider: &PostProcessProvider,
    secret: Option<&SecretValue>,
) -> Result<reqwest::Client, PostProcessModelDiscovery> {
    create_client(provider, secret).map_err(|_| {
        if secret.is_some() {
            PostProcessModelDiscovery::CredentialCorrupt
        } else {
            PostProcessModelDiscovery::InvalidResponse
        }
    })
}

async fn get_catalog_json<T: DeserializeOwned>(
    request: reqwest::RequestBuilder,
) -> Result<T, PostProcessModelDiscovery> {
    let response = request.send().await.map_err(|error| {
        let discovery = catalog_request_error(&error);
        error!(
            "Post-processing model discovery request failed (kind: {})",
            reqwest_error_kinds(&error)
        );
        discovery
    })?;
    let status = response.status();
    debug!(
        "Post-processing model catalog response received with status {} over {:?}",
        status,
        response.version()
    );
    if !status.is_success() {
        return Err(catalog_status_error(status));
    }

    let bytes = read_limited_catalog_response(response).await?;
    serde_json::from_slice(&bytes).map_err(|_| {
        error!("Failed to parse post-processing model catalog (kind: json)");
        PostProcessModelDiscovery::InvalidResponse
    })
}

async fn read_limited_catalog_response(
    response: reqwest::Response,
) -> Result<Vec<u8>, PostProcessModelDiscovery> {
    let declared_length = response
        .content_length()
        .map(|length| {
            usize::try_from(length).map_err(|_| PostProcessModelDiscovery::InvalidResponse)
        })
        .transpose()?;
    if declared_length.is_some_and(|length| length > MAX_CATALOG_RESPONSE_BYTES) {
        return Err(PostProcessModelDiscovery::InvalidResponse);
    }

    let mut bytes = Vec::with_capacity(declared_length.unwrap_or_default());
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            let discovery = catalog_request_error(&error);
            error!(
                "Post-processing model catalog body read failed (kind: {})",
                reqwest_error_kinds(&error)
            );
            discovery
        })?;
        let next_length = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or(PostProcessModelDiscovery::InvalidResponse)?;
        if next_length > MAX_CATALOG_RESPONSE_BYTES {
            return Err(PostProcessModelDiscovery::InvalidResponse);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn catalog_request_error(error: &reqwest::Error) -> PostProcessModelDiscovery {
    if error.is_timeout() || error.is_connect() || error.is_request() || error.is_redirect() {
        PostProcessModelDiscovery::Unreachable
    } else {
        PostProcessModelDiscovery::InvalidResponse
    }
}

fn catalog_status_error(status: reqwest::StatusCode) -> PostProcessModelDiscovery {
    match status.as_u16() {
        401 => PostProcessModelDiscovery::Unauthorized,
        403 => PostProcessModelDiscovery::Forbidden,
        404 => PostProcessModelDiscovery::InvalidDestination,
        408 | 425 | 500..=599 => PostProcessModelDiscovery::Unreachable,
        429 => PostProcessModelDiscovery::RateLimited,
        300..=399 => PostProcessModelDiscovery::Unreachable,
        _ => PostProcessModelDiscovery::InvalidResponse,
    }
}

/// Provider-reported ids reach settings, the model list, and later request
/// bodies, so an empty, overlong, or non-printable-ASCII id is not offered.
/// One unusable id costs its own row rather than the whole catalog.
fn catalog_id_is_safe(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_CATALOG_MODEL_ID_BYTES
        && id.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Collect up to `MAX_CATALOG_MODELS` options, then stop. The bound keeps one
/// endpoint from filling memory; truncating keeps that bound and still returns
/// a usable list. OpenRouter alone publishes several hundred text models, and
/// refusing the response would empty the picker for a shipped provider.
fn append_provider_reported_model(
    models: &mut Vec<PostProcessModelOption>,
    seen: &mut HashSet<String>,
    id: &str,
) {
    if models.len() >= MAX_CATALOG_MODELS || !seen.insert(id.to_string()) {
        return;
    }
    models.push(PostProcessModelOption {
        id: id.to_string(),
        provenance: PostProcessModelProvenance::ProviderReported,
    });
}

/// OpenAI's model list advertises availability, not a per-model capability
/// contract. Keep only the documented GPT text-generation families; a newer
/// or unknown ID remains manually enterable instead of being guessed at.
fn openai_model_is_eligible(id: &str) -> bool {
    id.starts_with("gpt-4o") || id.starts_with("gpt-4.1") || id.starts_with("gpt-5")
}

/// OpenRouter publishes modality metadata. Both text input and text output are
/// required before a listed model is offered for transcript post-processing.
fn openrouter_architecture_supports_text_io(architecture: &OpenRouterArchitecture) -> bool {
    architecture
        .input_modalities
        .iter()
        .any(|modality| modality.eq_ignore_ascii_case("text"))
        && architecture
            .output_modalities
            .iter()
            .any(|modality| modality.eq_ignore_ascii_case("text"))
}

/// Groq's list does not carry a modality contract, so this mirrors only its
/// documented chat-capable model families. Guard models are deliberately out.
fn groq_model_is_eligible(id: &str) -> bool {
    !id.contains("guard")
        && (id.starts_with("llama-")
            || id.starts_with("meta-llama/")
            || id.starts_with("qwen")
            || id.starts_with("openai/gpt-oss-")
            || id.starts_with("moonshotai/kimi-"))
}

/// Cerebras likewise lists availability rather than a negotiated capability;
/// retain only its documented instruction/chat families and leave the rest
/// available through manual entry.
fn cerebras_model_is_eligible(id: &str) -> bool {
    !id.contains("guard")
        && (id.starts_with("llama")
            || id.starts_with("qwen")
            || id.starts_with("gpt-oss")
            || id.starts_with("zai-glm"))
}

fn anthropic_model_is_eligible(id: &str) -> bool {
    id.starts_with("claude-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::{MemorySecretBackend, SecretAccount, SecretManager, SecretRead};
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn provider(id: &str, base_url: &str) -> PostProcessProvider {
        PostProcessProvider {
            id: id.to_string(),
            label: id.to_string(),
            base_url: base_url.to_string(),
            allow_base_url_edit: true,
            supports_structured_output: false,
        }
    }

    fn endpoint(provider: &PostProcessProvider) -> PostProcessEndpoint {
        provider.endpoint().expect("test provider endpoint")
    }

    async fn test_secret(provider_id: &str, value: &str) -> SecretValue {
        let backend = Arc::new(MemorySecretBackend::new());
        backend.insert(&format!("llm/{provider_id}"), value);
        let manager = SecretManager::with_backend(backend);
        let account = SecretAccount::llm(provider_id).expect("valid test account");
        match manager
            .resolve_optional(account)
            .await
            .expect("test secret resolves")
        {
            SecretRead::Found(secret) => secret,
            SecretRead::NotFound => panic!("test secret was not stored"),
        }
    }

    fn reported_models(ids: &[&str]) -> Vec<PostProcessModelOption> {
        ids.iter()
            .map(|id| PostProcessModelOption {
                id: (*id).to_string(),
                provenance: PostProcessModelProvenance::ProviderReported,
            })
            .collect()
    }

    #[test]
    fn structured_output_schema_preserves_schema_bytes() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
        });
        let expected = serde_json::to_vec(&schema).expect("schema serializes");
        let encoded =
            serde_json::to_vec(&StructuredOutputSchema(schema)).expect("typed schema serializes");

        assert_eq!(encoded, expected);
    }

    #[derive(Debug)]
    struct RequestJson(serde_json::Value);

    impl std::ops::Deref for RequestJson {
        type Target = serde_json::Value;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    fn request_json(reasoning: ReasoningParams) -> RequestJson {
        let request = ChatCompletionRequest {
            model: "test-model".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "hi".to_string(),
            }],
            stream: false,
            response_format: None,
            reasoning,
        };
        RequestJson(serde_json::to_value(&request).unwrap())
    }

    async fn serve_one_response(status: &str, body: &str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await.unwrap();
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        format!("http://{address}")
    }

    async fn serve_raw_response(response: Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await;
            let _ = stream.write_all(&response).await;
        });

        format!("http://{address}")
    }

    #[tokio::test]
    async fn failure_diagnostics_exclude_transcript_and_endpoint_canaries() {
        const CANARY: &str = "TRANSCRIPT-CANARY-4EE1";
        let base_url = serve_one_response("400 Bad Request", CANARY).await;
        let error = reqwest::get(format!("{base_url}/private?token={CANARY}"))
            .await
            .expect("request")
            .error_for_status()
            .expect_err("400 response");

        let details = report_reqwest_error("Request failed", &error);
        assert!(details.contains("kind: status"));
        assert!(!details.contains(CANARY));
        assert!(!details.contains(&base_url));

        let decode_url =
            serve_one_response("200 OK", &format!(r#"{{"choices":"{CANARY}"}}"#)).await;
        let decode_error = reqwest::get(decode_url)
            .await
            .expect("request")
            .json::<ChatCompletionResponse>()
            .await
            .expect_err("malformed response");
        let decode_details = report_reqwest_error("Failed to parse API response", &decode_error);
        assert!(decode_details.contains("kind: decode"));
        assert!(!decode_details.contains(CANARY));
    }

    /// A rejected reasoning request is retried even when the server closes its
    /// discarded error body early. The body has no semantic use, so a read
    /// failure must not turn a recoverable 400 into a failed transcription.
    #[tokio::test]
    async fn retries_after_the_discarded_reasoning_rejection_body_fails_to_read() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind retry server");
        let address = listener.local_addr().expect("retry server address");
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.expect("first request");
            let mut request = [0_u8; 4096];
            let _ = first.read(&mut request).await.expect("read first request");
            // Claim a longer body, write only part of it, then close. Reqwest
            // receives the 400 headers, but response.text() returns an error.
            first
                .write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 32\r\nConnection: close\r\n\r\nbad",
                )
                .await
                .expect("write truncated rejection");
            first.shutdown().await.expect("close rejection");

            let (mut retry, _) = listener.accept().await.expect("retry request");
            let _ = retry.read(&mut request).await.expect("read retry request");
            let body = br#"{"choices":[{"message":{"content":"retried"}}]}"#;
            retry
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write retry headers");
            retry.write_all(body).await.expect("write retry body");
        });

        let provider = provider("custom", &format!("http://{address}"));
        let endpoint = endpoint(&provider);
        let result = send_chat_completion(
            &provider,
            &endpoint,
            None,
            "retry-after-truncated-rejection",
            "transcribe this".to_string(),
            true,
        )
        .await;

        assert_eq!(result, Ok(Some("retried".to_string())));
        server.await.expect("retry server completed");
    }

    #[test]
    fn requests_explicitly_disable_streaming() {
        let json = request_json(ReasoningParams::default());
        assert_eq!(json["stream"], false);
    }

    #[test]
    fn default_reasoning_params_serialize_to_no_fields() {
        let json = request_json(ReasoningParams::default());
        assert!(json.get("reasoning_effort").is_none());
        assert!(json.get("reasoning").is_none());
        assert!(json.get("thinking").is_none());
    }

    #[test]
    fn custom_provider_uses_top_level_reasoning_effort() {
        let provider = provider("custom", "http://localhost:11434/v1");
        let params = reasoning_disable_params(&provider, &endpoint(&provider));
        let json = request_json(params);
        assert_eq!(json["reasoning_effort"], "none");
        assert!(json.get("reasoning").is_none());
        assert!(json.get("thinking").is_none());
    }

    #[test]
    fn openrouter_uses_nested_reasoning_object() {
        let provider = provider("openrouter", "https://openrouter.ai/api/v1");
        let params = reasoning_disable_params(&provider, &endpoint(&provider));
        let json = request_json(params);
        assert!(json.get("reasoning_effort").is_none());
        assert_eq!(json["reasoning"]["effort"], "none");
        assert_eq!(json["reasoning"]["exclude"], true);
        assert!(json.get("thinking").is_none());
    }

    #[test]
    fn deepseek_base_url_uses_thinking_disabled() {
        let provider = provider("custom", "https://api.deepseek.com");
        let params = reasoning_disable_params(&provider, &endpoint(&provider));
        let json = request_json(params);
        assert!(json.get("reasoning_effort").is_none());
        assert!(json.get("reasoning").is_none());
        assert_eq!(json["thinking"]["type"], "disabled");
    }

    #[test]
    fn reasoning_params_is_empty_tracks_all_fields() {
        assert!(ReasoningParams::default().is_empty());
        assert!(!ReasoningParams {
            reasoning_effort: Some("none".to_string()),
            ..Default::default()
        }
        .is_empty());
        assert!(!ReasoningParams {
            thinking: Some(ThinkingParams { kind: "disabled" }),
            ..Default::default()
        }
        .is_empty());
    }

    #[test]
    fn rejection_memo_is_keyed_by_endpoint_and_model() {
        let deepseek = provider("custom", "https://api.deepseek.com/");
        let endpoint = endpoint(&deepseek);
        let key = endpoint_key(&endpoint, "deepseek-chat");
        assert_eq!(key, "https://api.deepseek.com|deepseek-chat");
        assert!(!is_known_rejected(&key));
        remember_rejection(key.clone());
        assert!(is_known_rejected(&key));
        // A different model on the same endpoint is tracked separately.
        assert!(!is_known_rejected(&endpoint_key(&endpoint, "other-model")));
    }

    #[tokio::test]
    async fn redirects_do_not_forward_authorization() {
        let target = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind redirect target");
        let target_address = target.local_addr().expect("target address");
        let source = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind redirect source");
        let source_address = source.local_addr().expect("source address");
        let source_server = tokio::spawn(async move {
            let (mut stream, _) = source.accept().await.expect("source request");
            let mut request = [0_u8; 2048];
            let _ = stream
                .read(&mut request)
                .await
                .expect("read source request");
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://{target_address}/redirected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write redirect");
        });

        let provider = provider("custom", &format!("http://{source_address}"));
        let endpoint = endpoint(&provider);
        let response = create_client(&provider, None)
            .expect("client")
            .post(endpoint.request_url("chat/completions"))
            .header(
                reqwest::header::AUTHORIZATION,
                "Bearer TRANSCRIPT-CANARY-4EE1",
            )
            .send()
            .await
            .expect("redirect response");

        assert_eq!(response.status().as_u16(), 302);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), target.accept())
                .await
                .is_err()
        );
        source_server.await.expect("source completed");
    }

    #[tokio::test]
    async fn frozen_destination_rejects_changed_provider_before_request() {
        const CANARY: &str = "TRANSCRIPT-CANARY-4EE1";
        let original = provider("custom", "http://127.0.0.1:31001/v1");
        let frozen = endpoint(&original);
        let changed = provider("custom", "http://127.0.0.1:31002/v1");

        let error = send_chat_completion(
            &changed,
            &frozen,
            None,
            "test-model",
            CANARY.to_string(),
            false,
        )
        .await
        .expect_err("changed endpoint must be rejected");

        assert_eq!(error, "Post-processing destination changed");
        assert!(!error.contains(CANARY));
    }

    /// The JSON body of a captured HTTP request; empty until the headers end.
    fn request_body(request: &[u8]) -> &[u8] {
        match request.windows(4).position(|window| window == b"\r\n\r\n") {
            Some(index) => &request[index + 4..],
            None => &[],
        }
    }

    /// Answers one chat completion and hands back the request it received.
    async fn serve_one_completion(content: &str) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind completion server");
        let address = listener.local_addr().expect("completion server address");
        let body =
            serde_json::json!({ "choices": [{ "message": { "content": content } }] }).to_string();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("completion request");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            // Read until the body parses, not just until the first segment: a
            // prompt this size does not arrive in one.
            loop {
                let read = stream.read(&mut chunk).await.expect("read request");
                request.extend_from_slice(&chunk[..read]);
                if read == 0
                    || serde_json::from_slice::<serde_json::Value>(request_body(&request)).is_ok()
                {
                    break;
                }
            }
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write completion");
            request
        });
        (format!("http://{address}"), server)
    }

    /// A dictation that ended with `Sona, …` reaches the provider as an
    /// instruction over an input, never as one concatenated prompt: an input
    /// that says "ignore the above" must not be able to direct the edit. The
    /// answer is the whole delivery, which is why it is read back verbatim.
    #[tokio::test]
    async fn a_spoken_instruction_rides_as_instruction_plus_input() {
        let (base_url, server) = serve_one_completion("The plan is ready by Friday?").await;
        let provider = provider("custom", &base_url);
        let endpoint = endpoint(&provider);
        let rendered = crate::prompt_renderer::render_instruction(
            crate::prompt_renderer::InstructionRenderInput {
                instruction: "make that a question.",
                input: "The plan is ready by Friday.",
                language: "en",
                target: &crate::context::TargetMetadata::default(),
            },
        );

        let answer = send_chat_completion_with_schema(ChatCompletionInput {
            provider: &provider,
            endpoint: &endpoint,
            secret: None,
            model: "spoken-instruction",
            user_content: rendered.user_message.clone(),
            system_prompt: Some(rendered.system_message.clone()),
            json_schema: None,
            disable_reasoning: false,
        })
        .await;

        assert_eq!(answer, Ok(Some("The plan is ready by Friday?".to_string())));

        let request = server.await.expect("completion server finished");
        let sent: serde_json::Value =
            serde_json::from_slice(request_body(&request)).expect("the request body is JSON");
        assert_eq!(sent["messages"][0]["role"], "system");
        assert_eq!(sent["messages"][1]["role"], "user");
        let envelope: serde_json::Value = serde_json::from_str(
            sent["messages"][1]["content"]
                .as_str()
                .expect("user content"),
        )
        .expect("the user message is a JSON envelope");
        assert_eq!(envelope["instruction"], "make that a question.");
        assert_eq!(envelope["input"], "The plan is ready by Friday.");
    }

    /// A failed status has to arrive as an error, not as text: every caller
    /// substitutes only what the provider returned, so anything else would
    /// deliver a failure message in place of the user's dictation. This is the
    /// one test that pins that mapping through `send_chat_completion`.
    #[tokio::test]
    async fn a_failed_status_is_an_error_not_text() {
        let base_url = serve_one_response("500 Internal Server Error", "upstream is down").await;
        let provider = provider("custom", &base_url);
        let endpoint = endpoint(&provider);

        let answer = send_chat_completion_with_schema(ChatCompletionInput {
            provider: &provider,
            endpoint: &endpoint,
            secret: None,
            model: "any",
            user_content: "{}".to_string(),
            system_prompt: None,
            json_schema: None,
            disable_reasoning: false,
        })
        .await;

        assert_eq!(
            answer,
            Err("API request failed with status 500 Internal Server Error".to_string())
        );
    }

    /// Anthropic is not OpenAI-compatible at this boundary: the transport
    /// route, request shape, and text response block are its native protocol.
    #[tokio::test]
    async fn anthropic_messages_use_native_path_and_text_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind Anthropic fixture");
        let address = listener.local_addr().expect("Anthropic fixture address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("Anthropic request");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let read = stream
                    .read(&mut chunk)
                    .await
                    .expect("read Anthropic request");
                request.extend_from_slice(&chunk[..read]);
                if read == 0
                    || serde_json::from_slice::<serde_json::Value>(request_body(&request)).is_ok()
                {
                    break;
                }
            }
            let body = br#"{"content":[{"type":"text","text":"rewritten"}]}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write Anthropic headers");
            stream.write_all(body).await.expect("write Anthropic body");
            request
        });

        let provider = provider("anthropic", &format!("http://{address}/v1"));
        let endpoint = endpoint(&provider);
        let answer = send_chat_completion_with_schema(ChatCompletionInput {
            provider: &provider,
            endpoint: &endpoint,
            secret: None,
            model: "claude-test",
            user_content: "rewrite this".to_string(),
            system_prompt: Some("keep punctuation".to_string()),
            json_schema: None,
            disable_reasoning: true,
        })
        .await;

        assert_eq!(answer, Ok(Some("rewritten".to_string())));
        let request = server.await.expect("Anthropic fixture completed");
        let request_text = String::from_utf8(request).expect("request is UTF-8");
        assert!(request_text.starts_with("POST /v1/messages HTTP/1.1\r\n"));
        let sent: serde_json::Value = serde_json::from_slice(request_body(request_text.as_bytes()))
            .expect("Anthropic request body is JSON");
        assert_eq!(sent["model"], "claude-test");
        assert_eq!(sent["max_tokens"], 4096);
        assert_eq!(sent["system"], "keep punctuation");
        assert_eq!(sent["messages"][0]["role"], "user");
        assert_eq!(sent["messages"][0]["content"], "rewrite this");
    }

    #[tokio::test]
    async fn openai_catalog_keeps_documented_text_models_and_dedupes() {
        let base_url = serve_one_response(
            "200 OK",
            r#"{"data":[{"id":"gpt-4o-mini"},{"id":"text-embedding-3-large"},{"id":"gpt-4o-mini"},{"id":"gpt-5-mini"},{"id":"whisper-1"}]}"#,
        )
        .await;
        let provider = provider("openai", &base_url);

        let models = discover_models(&provider, &endpoint(&provider), None)
            .await
            .expect("OpenAI catalog response");

        assert_eq!(models, reported_models(&["gpt-4o-mini", "gpt-5-mini"]));
    }

    #[tokio::test]
    async fn openrouter_catalog_requires_text_input_and_output_metadata() {
        let base_url = serve_one_response(
            "200 OK",
            r#"{"data":[{"id":"vendor/text","architecture":{"input_modalities":["text"],"output_modalities":["text"]}},{"id":"vendor/image","architecture":{"input_modalities":["image"],"output_modalities":["text"]}},{"id":"vendor/unknown"}]}"#,
        )
        .await;
        let provider = provider("openrouter", &base_url);

        let models = discover_models(&provider, &endpoint(&provider), None)
            .await
            .expect("OpenRouter catalog response");

        assert_eq!(models, reported_models(&["vendor/text"]));
    }

    #[tokio::test]
    async fn groq_and_cerebras_catalogs_filter_unproven_models() {
        let groq_base = serve_one_response(
            "200 OK",
            r#"{"data":[{"id":"llama-3.3-70b-versatile"},{"id":"llama-guard-4-12b"},{"id":"text-embedding-3-large"}]}"#,
        )
        .await;
        let groq = provider("groq", &groq_base);
        let groq_models = discover_models(&groq, &endpoint(&groq), None)
            .await
            .expect("Groq catalog response");
        assert_eq!(groq_models, reported_models(&["llama-3.3-70b-versatile"]));

        let cerebras_base = serve_one_response(
            "200 OK",
            r#"{"data":[{"id":"llama3.1-8b"},{"id":"qwen-3-235b-a22b-instruct-2507"},{"id":"text-embedding-3-large"}]}"#,
        )
        .await;
        let cerebras = provider("cerebras", &cerebras_base);
        let cerebras_models = discover_models(&cerebras, &endpoint(&cerebras), None)
            .await
            .expect("Cerebras catalog response");
        assert_eq!(
            cerebras_models,
            reported_models(&["llama3.1-8b", "qwen-3-235b-a22b-instruct-2507"])
        );
    }

    #[tokio::test]
    async fn custom_catalog_exposes_only_server_reported_suggestions() {
        let base_url = serve_one_response(
            "200 OK",
            r#"{"data":[{"id":"gemma3:12b"},{"id":"gemma3:12b"}]}"#,
        )
        .await;
        let provider = provider("custom", &base_url);

        let models = discover_models(&provider, &endpoint(&provider), None)
            .await
            .expect("custom catalog response");

        assert_eq!(models, reported_models(&["gemma3:12b"]));
    }

    /// A self-hosted server can list one oddly named artefact beside its chat
    /// models. That row is dropped; the rest of the catalog still reaches the
    /// picker.
    #[tokio::test]
    async fn catalog_skips_an_unsafe_model_id_and_keeps_the_rest() {
        const BODY_CANARY: &str = "MODEL-BODY-CANARY-9B88";
        let body = format!(
            r#"{{"data":[{{"id":"{}{}"}},{{"id":"café-13b"}},{{"id":""}},{{"id":"gemma3:12b"}}]}}"#,
            "a".repeat(MAX_CATALOG_MODEL_ID_BYTES),
            BODY_CANARY
        );
        let base_url = serve_one_response("200 OK", &body).await;
        let provider = provider("custom", &base_url);

        let models = discover_models(&provider, &endpoint(&provider), None)
            .await
            .expect("a catalog carrying one unusable id still resolves");

        assert_eq!(models, reported_models(&["gemma3:12b"]));
        assert!(!format!("{models:?}").contains(BODY_CANARY));
    }

    /// OpenRouter alone publishes several hundred text models. The cap bounds
    /// what one endpoint can spend, so crossing it truncates instead of
    /// throwing away a catalog the user would have picked from.
    #[tokio::test]
    async fn catalog_over_the_model_cap_is_truncated_not_refused() {
        let entries: Vec<String> = (0..MAX_CATALOG_MODELS + 5)
            .map(|index| format!(r#"{{"id":"model-{index:04}"}}"#))
            .collect();
        let body = format!(r#"{{"data":[{}]}}"#, entries.join(","));
        let base_url = serve_one_response("200 OK", &body).await;
        let provider = provider("custom", &base_url);

        let models = discover_models(&provider, &endpoint(&provider), None)
            .await
            .expect("a catalog past the cap still resolves");

        assert_eq!(models.len(), MAX_CATALOG_MODELS);
        assert_eq!(models.first().expect("first option").id, "model-0000");
        assert_eq!(
            models.last().expect("last option").id,
            format!("model-{:04}", MAX_CATALOG_MODELS - 1)
        );
    }

    /// A reply Anthropic clipped at the output ceiling must not reach the user
    /// as though it were whole: the caller reads an error as "no rewrite" and
    /// keeps the raw transcript.
    #[tokio::test]
    async fn anthropic_reply_clipped_at_the_token_ceiling_is_refused() {
        const CLIPPED: &str = "half a rewritten senten";
        let base_url = serve_one_response(
            "200 OK",
            &format!(
                r#"{{"content":[{{"type":"text","text":"{CLIPPED}"}}],"stop_reason":"max_tokens"}}"#
            ),
        )
        .await;
        let provider = provider("anthropic", &format!("{base_url}/v1"));
        let endpoint = endpoint(&provider);

        let answer = send_chat_completion_with_schema(ChatCompletionInput {
            provider: &provider,
            endpoint: &endpoint,
            secret: None,
            model: "claude-test",
            user_content: "rewrite this".to_string(),
            system_prompt: None,
            json_schema: None,
            disable_reasoning: true,
        })
        .await;

        let error = answer.expect_err("a clipped reply is refused");
        assert!(!error.contains(CLIPPED));
    }

    #[tokio::test]
    async fn catalog_rejects_declared_response_over_byte_ceiling_without_waiting_for_body() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind oversized catalog fixture");
        let address = listener
            .local_addr()
            .expect("oversized catalog fixture address");
        let (release_sender, release_receiver) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("catalog request");
            let mut request = [0_u8; 2048];
            stream
                .read(&mut request)
                .await
                .expect("read catalog request");
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_CATALOG_RESPONSE_BYTES + 1
            );
            stream
                .write_all(headers.as_bytes())
                .await
                .expect("write oversized catalog headers");
            let _ = release_receiver.await;
        });
        let provider = provider("custom", &format!("http://{address}"));

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            discover_models(&provider, &endpoint(&provider), None),
        )
        .await
        .expect("declared catalog limit rejects before reading the body");

        assert_eq!(result, Err(PostProcessModelDiscovery::InvalidResponse));
        drop(release_sender);
        server.await.expect("oversized catalog fixture completed");
    }

    #[tokio::test]
    async fn catalog_rejects_chunked_response_over_byte_ceiling() {
        const BODY_CANARY: &str = "CHUNKED-CATALOG-BODY-CANARY-B2AF";
        let metadata = format!(r#"{{"data":[],"padding":"{BODY_CANARY}"}}"#);
        let body = vec![b' '; MAX_CATALOG_RESPONSE_BYTES];
        let mut response =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
        for chunk in [metadata.as_bytes(), body.as_slice()] {
            response.extend_from_slice(format!("{:X}\r\n", chunk.len()).as_bytes());
            response.extend_from_slice(chunk);
            response.extend_from_slice(b"\r\n");
        }
        response.extend_from_slice(b"0\r\n\r\n");
        let base_url = serve_raw_response(response).await;
        let provider = provider("custom", &base_url);

        let result = discover_models(&provider, &endpoint(&provider), None).await;

        assert_eq!(result, Err(PostProcessModelDiscovery::InvalidResponse));
        assert!(!format!("{result:?}").contains(BODY_CANARY));
    }

    #[tokio::test]
    async fn catalog_rejects_decompressed_response_over_byte_ceiling() {
        const BODY_CANARY: &str = "COMPRESSED-CATALOG-BODY-CANARY-5FE2";
        let mut decoded = format!(r#"{{"data":[],"padding":"{BODY_CANARY}"}}"#).into_bytes();
        decoded.resize(MAX_CATALOG_RESPONSE_BYTES + 1, b' ');
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder
            .write_all(&decoded)
            .expect("compress catalog response fixture");
        let compressed = encoder.finish().expect("finish catalog response fixture");
        assert!(compressed.len() < MAX_CATALOG_RESPONSE_BYTES);

        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            compressed.len()
        )
        .into_bytes();
        response.extend_from_slice(&compressed);
        let base_url = serve_raw_response(response).await;
        let provider = provider("custom", &base_url);

        let result = discover_models(&provider, &endpoint(&provider), None).await;

        assert_eq!(result, Err(PostProcessModelDiscovery::InvalidResponse));
        assert!(!format!("{result:?}").contains(BODY_CANARY));
    }
    async fn catalog_status(
        status: &str,
    ) -> Result<Vec<PostProcessModelOption>, PostProcessModelDiscovery> {
        let base_url = serve_one_response(status, "STATUS-BODY-CANARY").await;
        let provider = provider("custom", &base_url);
        discover_models(&provider, &endpoint(&provider), None).await
    }

    macro_rules! catalog_status_test {
        ($name:ident, $status:literal, $expected:expr) => {
            #[tokio::test]
            async fn $name() {
                assert_eq!(catalog_status($status).await, Err($expected));
            }
        };
    }

    catalog_status_test!(
        catalog_classifies_unauthorized,
        "401 Unauthorized",
        PostProcessModelDiscovery::Unauthorized
    );
    catalog_status_test!(
        catalog_classifies_forbidden,
        "403 Forbidden",
        PostProcessModelDiscovery::Forbidden
    );
    catalog_status_test!(
        catalog_classifies_rate_limit,
        "429 Too Many Requests",
        PostProcessModelDiscovery::RateLimited
    );
    catalog_status_test!(
        catalog_classifies_outage,
        "503 Service Unavailable",
        PostProcessModelDiscovery::Unreachable
    );

    #[tokio::test]
    async fn anthropic_catalog_pages_with_native_headers_and_cursor() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind Anthropic catalog fixture");
        let address = listener.local_addr().expect("Anthropic catalog address");
        let server = tokio::spawn(async move {
            let pages = [
                r#"{"data":[{"id":"claude-3-5-haiku-latest"}],"has_more":true,"last_id":"claude-3-5-haiku-latest"}"#,
                r#"{"data":[{"id":"claude-sonnet-4-20250514"}],"has_more":false,"last_id":"claude-sonnet-4-20250514"}"#,
            ];
            let mut requests = Vec::new();
            for body in pages {
                let (mut stream, _) =
                    tokio::time::timeout(Duration::from_secs(2), listener.accept())
                        .await
                        .expect("Anthropic catalog client did not request next page")
                        .expect("accept Anthropic catalog request");
                let mut request = [0_u8; 4096];
                let count = stream
                    .read(&mut request)
                    .await
                    .expect("read Anthropic catalog request");
                requests.push(request[..count].to_vec());
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .expect("write Anthropic catalog page");
            }
            requests
        });

        let provider = provider("anthropic", &format!("http://{address}/v1"));
        let secret = test_secret("anthropic", "ANTHROPIC-KEY-CANARY").await;
        let models = discover_models(&provider, &endpoint(&provider), Some(&secret))
            .await
            .expect("Anthropic paginated catalog");

        assert_eq!(
            models,
            reported_models(&["claude-3-5-haiku-latest", "claude-sonnet-4-20250514"])
        );
        let requests = server.await.expect("Anthropic catalog fixture completed");
        let first = String::from_utf8(requests[0].clone()).expect("first request is UTF-8");
        let second = String::from_utf8(requests[1].clone()).expect("second request is UTF-8");
        assert!(first.starts_with("GET /v1/models HTTP/1.1\r\n"));
        assert!(first.contains("x-api-key: ANTHROPIC-KEY-CANARY\r\n"));
        assert!(first.contains("anthropic-version: 2023-06-01\r\n"));
        assert!(!first.contains("after_id="));
        assert!(second.starts_with("GET /v1/models?after_id=claude-3-5-haiku-latest HTTP/1.1\r\n"));
        assert!(second.contains("x-api-key: ANTHROPIC-KEY-CANARY\r\n"));
        assert!(second.contains("anthropic-version: 2023-06-01\r\n"));
    }

    #[tokio::test]
    async fn unsupported_catalog_sources_do_not_open_a_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind no-connection fixture");
        let address = listener
            .local_addr()
            .expect("no-connection fixture address");
        for provider_id in ["zai", "bedrock_mantle"] {
            let provider = provider(provider_id, &format!("http://{address}/v1"));
            assert_eq!(
                discover_models(&provider, &endpoint(&provider), None).await,
                Err(PostProcessModelDiscovery::Unsupported)
            );
        }
        let apple = provider("apple_intelligence", "apple-intelligence://local");
        assert_eq!(
            discover_models(&apple, &endpoint(&apple), None).await,
            Err(PostProcessModelDiscovery::Unsupported)
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "unsupported discovery opened a network connection"
        );
    }

    #[tokio::test]
    async fn catalog_redirects_are_not_followed() {
        let target = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind catalog redirect target");
        let target_address = target
            .local_addr()
            .expect("catalog redirect target address");
        let source = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind catalog redirect source");
        let source_address = source
            .local_addr()
            .expect("catalog redirect source address");
        let source_server = tokio::spawn(async move {
            let (mut stream, _) = source.accept().await.expect("catalog redirect request");
            let mut request = [0_u8; 2048];
            let _ = stream
                .read(&mut request)
                .await
                .expect("read catalog redirect request");
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: http://{target_address}/models\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .expect("write catalog redirect");
        });

        let provider = provider("custom", &format!("http://{source_address}"));
        assert_eq!(
            discover_models(&provider, &endpoint(&provider), None).await,
            Err(PostProcessModelDiscovery::Unreachable)
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), target.accept())
                .await
                .is_err(),
            "catalog client followed a redirect"
        );
        source_server
            .await
            .expect("catalog redirect source completed");
    }

    /// Proves a keyless local endpoint end to end through this client, because
    /// no fixture can: every other test here serves its own canned bytes, so
    /// none of them can show that a real OpenAI-compatible server accepts what
    /// we send with no `Authorization` header at all.
    ///
    /// Ignored by default — it needs something listening on 11434. Run it with
    /// `bun run test:backend ollama -- --ignored --nocapture`, and set
    /// `SONA_LOCAL_MODEL` if the served model is not the one below.
    ///
    /// The provider fields are the shipped `custom` defaults verbatim
    /// (`settings.rs` `default_post_process_providers`), the credential is
    /// `None` exactly as `provider_allows_unauthenticated_request` makes it for
    /// a loopback `custom` route, `disable_reasoning` is true exactly as
    /// `post_process_transcription` sets it, and the prompt is a real rendered
    /// command-mode pair concatenated the same way. So a pass here is a pass
    /// for the production path, not for a lookalike.
    #[tokio::test]
    #[ignore = "requires a local OpenAI-compatible server on 127.0.0.1:11434"]
    async fn a_keyless_loopback_endpoint_rewrites_a_selection() {
        let provider = PostProcessProvider {
            id: "custom".to_string(),
            label: "Custom".to_string(),
            base_url: "http://localhost:11434/v1".to_string(),
            allow_base_url_edit: true,
            supports_structured_output: false,
        };
        let endpoint = endpoint(&provider);
        // The whole reason no key is needed: a loopback route is not remote, so
        // neither the consent gate nor the credential lookup applies.
        assert!(!endpoint.is_remote());

        // The loopback catalog is keyless on the same terms as execution. Its
        // entries are server-reported suggestions, not a compatibility claim.
        let listed = discover_models(&provider, &endpoint, None)
            .await
            .expect("the local endpoint listed its models");
        println!("models: {listed:?}");
        assert!(!listed.is_empty());
        assert!(listed
            .iter()
            .all(|model| { model.provenance == PostProcessModelProvenance::ProviderReported }));

        let model = std::env::var("SONA_LOCAL_MODEL").unwrap_or_else(|_| "gemma4:12b-mlx".into());
        let rendered = crate::prompt_renderer::render_instruction(
            crate::prompt_renderer::InstructionRenderInput {
                instruction: "make that a question",
                input: "The plan is ready by Friday.",
                language: "en",
                target: &crate::context::TargetMetadata::default(),
            },
        );
        let prompt = format!("{}\n\n{}", rendered.system_message, rendered.user_message);

        let started = std::time::Instant::now();
        let answer = send_chat_completion(&provider, &endpoint, None, &model, prompt, true).await;
        let elapsed = started.elapsed();

        let text = answer
            .expect("the local endpoint answered")
            .expect("the answer carried content");
        println!("model: {model}");
        println!("latency: {elapsed:?}");
        println!("output: {text}");
        assert!(!text.trim().is_empty());
    }

    /// The check behind `REQUEST_TIMEOUT`. A refused connect already fails
    /// fast, so it proves nothing about the bound; the state that needed one is
    /// an endpoint that accepts the connection, reads the request, and then
    /// never answers — a local model still loading its weights, or a wedged
    /// server. Without the bound this hangs forever, because reqwest's default
    /// is no timeout.
    ///
    /// Ignored by default because passing costs the full 20 seconds. Run it
    /// with `bun run test:backend hangs -- --ignored --nocapture`. The
    /// elapsed
    /// assertion is the point: it fails if some other error path short-circuits
    /// instead, which would make the timeout untested while looking green.
    #[tokio::test]
    #[ignore = "waits out the full 20s request timeout"]
    async fn an_endpoint_that_never_answers_times_out() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await;
            // Hold the connection open and answer nothing.
            tokio::time::sleep(Duration::from_secs(120)).await;
        });

        let provider = provider("custom", &format!("http://{address}"));
        let endpoint = endpoint(&provider);
        let started = std::time::Instant::now();
        let answer =
            send_chat_completion(&provider, &endpoint, None, "any", "hi".into(), false).await;
        let elapsed = started.elapsed();
        println!("gave up after {elapsed:?}: {answer:?}");

        let error = answer.expect_err("a silent endpoint is an error, not text");
        assert!(error.contains("timeout"), "unexpected failure: {error}");
        assert!(elapsed >= REQUEST_TIMEOUT, "gave up early: {elapsed:?}");
        assert!(
            elapsed < REQUEST_TIMEOUT + Duration::from_secs(5),
            "outlasted the bound: {elapsed:?}"
        );
    }
}
