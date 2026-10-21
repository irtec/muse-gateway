use axum::{
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use std::sync::Arc;
use tower_http::trace::TraceLayer;
use tracing::{error, info};

use crate::config::AgentConfig;
use crate::models::{
    AnthropicRequest, ChatCompletionRequest, ImageGenerationRequest, UdsMediaRequest,
    UdsMediaResponse, UdsResponse,
};
use crate::supervisor::ProcessSupervisor;
use crate::translator::{
    convert_anthropic_to_uds, convert_openai_to_uds, extract_uds_content_text,
    format_uds_to_anthropic, format_uds_to_anthropic_stream, format_uds_to_openai,
    format_uds_to_openai_stream, normalize_tool_choice, parse_tool_calls,
};
use crate::uds::UdsClient;

#[derive(Clone)]
pub struct AppState {
    pub config: AgentConfig,
    pub uds_client: Arc<UdsClient>,
    pub media_client: Arc<UdsClient>,
    pub supervisor: Arc<ProcessSupervisor>,
}

pub fn create_agent_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/healthz", get(health_handler))
        .route("/v1/models", get(models_handler))
        .route("/models", get(models_handler))
        .route("/v1/chat/completions", post(openai_chat_completions))
        .route("/chat/completions", post(openai_chat_completions))
        .route("/v1/messages", post(anthropic_messages))
        .route("/messages", post(anthropic_messages))
        .route("/v1/images/generations", post(openai_image_generations))
        .route("/images/generations", post(openai_image_generations))
        .route("/internal/sync/manifest", get(internal_sync_manifest))
        .route("/internal/sync/download", get(internal_sync_download))
        .layer(DefaultBodyLimit::max(100 * 1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

fn check_auth(state: &AppState, headers: &HeaderMap) -> bool {
    if state.config.vm.token.is_empty() {
        return true;
    }

    if let Some(auth_val) = headers.get("authorization").and_then(|h| h.to_str().ok()) {
        let token = if let Some(stripped) = auth_val.strip_prefix("Bearer ") {
            stripped.trim()
        } else {
            auth_val.trim()
        };
        if token == state.config.vm.token {
            return true;
        }
    }

    if let Some(key) = headers.get("x-api-key").and_then(|h| h.to_str().ok()) {
        if key.trim() == state.config.vm.token {
            return true;
        }
    }

    if let Some(key) = headers.get("api-key").and_then(|h| h.to_str().ok()) {
        if key.trim() == state.config.vm.token {
            return true;
        }
    }

    false
}

async fn health_handler(State(state): State<AppState>) -> impl IntoResponse {
    let uds_ready = match state.uds_client.send_request(b"{\"ping\":true}").await {
        Ok(_) => true,
        Err(e) => {
            let err_str = e.to_string();
            !err_str.contains("Connection refused")
                && !err_str.contains("No such file or directory")
        }
    };

    // Subservice 1: AI Bridge is implicitly true since we are responding here
    let ai_bridge = true;

    // Subservice 2: SSH Daemon check (port 22)
    let sshd_ready = tokio::net::TcpStream::connect("127.0.0.1:22").await.is_ok();

    // Subservice 3: Storage / File Service check
    // If master: check local port 8080 (file manager)
    // If replica: check sync status
    let storage_ready = if state.config.file_manager.enabled {
        tokio::net::TcpStream::connect(format!("127.0.0.1:{}", state.config.file_manager.port))
            .await
            .is_ok()
    } else if state.config.sync.enabled {
        // Replica sync check: verify sync_dir exists
        std::path::Path::new(&state.config.sync.sync_dir).exists()
    } else {
        true
    };

    let all_subservices_ready = uds_ready && ai_bridge && sshd_ready && storage_ready;
    let (status_code, status_str) = if all_subservices_ready {
        (StatusCode::OK, "healthy")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "degraded")
    };

    (
        status_code,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        json!({
            "status": status_str,
            "vm_id": state.config.vm.id,
            "version": env!("CARGO_PKG_VERSION"),
            "role": if state.config.file_manager.enabled { "master" } else { "replica" },
            "uds_ready": uds_ready,
            "all_ready": all_subservices_ready,
            "subservices": {
                "ai_bridge": ai_bridge,
                "uds_socket": uds_ready,
                "sshd": sshd_ready,
                "storage": storage_ready
            },
            "update_error": state.supervisor.last_update_error(),
            "ok": all_subservices_ready
        })
        .to_string(),
    )
}

async fn models_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !check_auth(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json!({
                "error": {
                    "message": "Incorrect API key provided.",
                    "type": "invalid_request_error",
                    "code": "invalid_api_key"
                }
            })
            .to_string(),
        )
            .into_response();
    }

    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        json!({
            "object": "list",
            "data": [
                {
                    "id": "muse",
                    "object": "model",
                    "owned_by": format!("muse-{}", state.config.vm.id)
                }
            ]
        })
        .to_string(),
    )
        .into_response()
}

async fn openai_chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ChatCompletionRequest>,
) -> Response {
    if !check_auth(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json!({
                "error": {
                    "message": "Incorrect API key provided.",
                    "type": "invalid_request_error",
                    "code": "invalid_api_key"
                }
            })
            .to_string(),
        )
            .into_response();
    }

    let model_req = payload.model.clone();
    let uds_req = convert_openai_to_uds(&payload, "gateway");
    let uds_bytes = match serde_json::to_vec(&uds_req) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                json!({ "error": { "message": e.to_string(), "type": "invalid_request_error" } })
                    .to_string(),
            )
                .into_response();
        }
    };

    info!(
        "Forwarding OpenAI request to UDS (VM: {}, req_id: {})",
        state.config.vm.id, uds_req.request_id
    );

    match state.uds_client.send_request(&uds_bytes).await {
        Ok(resp_bytes) => match serde_json::from_slice::<UdsResponse>(&resp_bytes) {
            Ok(uds_resp) => {
                // Check if UDS returned an error
                if !uds_resp.ok {
                    let err_msg = uds_resp
                        .error
                        .as_ref()
                        .and_then(|e| e.as_str())
                        .unwrap_or("inference_failed");
                    error!(
                        "OpenAI req_id={}: UDS returned ok=false, error={}",
                        uds_req.request_id, err_msg
                    );
                    return (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        json!({
                            "error": {
                                "message": format!("Upstream inference error: {}", err_msg),
                                "type": "upstream_error",
                                "code": 422
                            }
                        })
                        .to_string(),
                    )
                        .into_response();
                }

                let (raw_text, _, _, _) = extract_uds_content_text(&uds_resp);
                let (mut is_tool, mut tool_calls, mut clean_text) =
                    if payload.tools.is_some() || payload.functions.is_some() {
                        parse_tool_calls(&raw_text)
                    } else {
                        (false, Vec::new(), Some(raw_text))
                    };

                // Tool retry nudge loop if tools are required or forced
                let (tc_mode, forced_name) = normalize_tool_choice(
                    payload
                        .tool_choice
                        .as_ref()
                        .or(payload.function_call.as_ref()),
                );
                let need_tool = tc_mode == "required" || tc_mode == "forced";

                let mut current_uds_resp = uds_resp;

                if (payload.tools.is_some() || payload.functions.is_some()) && need_tool {
                    let mut tool_satisfied = if tc_mode == "forced" && forced_name.is_some() {
                        let fname = forced_name.as_deref().unwrap_or_default();
                        is_tool && tool_calls.iter().any(|c| c.function.name == fname)
                    } else {
                        is_tool
                    };

                    let mut attempt = 1;
                    let max_tool_retries = 3;

                    while !tool_satisfied && attempt < max_tool_retries {
                        attempt += 1;
                        let nudge = "\n\nIMPORTANT: Your previous reply did not contain a tool call. You MUST respond with ONLY the JSON tool call object, nothing else.";
                        let mut retry_req = uds_req.clone();
                        let new_sys = match &retry_req.system {
                            Some(s) => format!("{}{}", s, nudge),
                            None => nudge.trim_start().to_string(),
                        };
                        retry_req.system = Some(new_sys);
                        retry_req.request_id = format!("gw-retry-{}", uuid::Uuid::new_v4());

                        if let Ok(retry_bytes) = serde_json::to_vec(&retry_req) {
                            if let Ok(retry_resp_bytes) =
                                state.uds_client.send_request(&retry_bytes).await
                            {
                                if let Ok(retry_uds_resp) =
                                    serde_json::from_slice::<UdsResponse>(&retry_resp_bytes)
                                {
                                    let (retry_raw, _, _, _) =
                                        extract_uds_content_text(&retry_uds_resp);
                                    let (r_is_tool, r_calls, r_clean) =
                                        parse_tool_calls(&retry_raw);
                                    is_tool = r_is_tool;
                                    tool_calls = r_calls;
                                    clean_text = r_clean;
                                    current_uds_resp = retry_uds_resp;

                                    tool_satisfied = if tc_mode == "forced" && forced_name.is_some()
                                    {
                                        let fname = forced_name.as_deref().unwrap_or_default();
                                        is_tool
                                            && tool_calls.iter().any(|c| c.function.name == fname)
                                    } else {
                                        is_tool
                                    };
                                }
                            }
                        }
                    }
                }

                if payload.stream == Some(true) {
                    let sse_str = format_uds_to_openai_stream(
                        &current_uds_resp,
                        model_req.as_deref(),
                        is_tool,
                        &tool_calls,
                        clean_text.as_deref(),
                    );
                    (
                        StatusCode::OK,
                        [
                            (
                                axum::http::header::CONTENT_TYPE.as_str(),
                                "text/event-stream; charset=utf-8",
                            ),
                            (
                                axum::http::header::CACHE_CONTROL.as_str(),
                                "no-cache, no-transform",
                            ),
                            (axum::http::header::CONNECTION.as_str(), "keep-alive"),
                            ("x-accel-buffering", "no"),
                        ],
                        sse_str,
                    )
                        .into_response()
                } else {
                    let openai_resp = format_uds_to_openai(
                        &current_uds_resp,
                        model_req.as_deref(),
                        is_tool,
                        &tool_calls,
                        clean_text.as_deref(),
                    );
                    (
                        StatusCode::OK,
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        serde_json::to_string(&openai_resp).unwrap_or_default(),
                    )
                        .into_response()
                }
            }
            Err(e) => {
                error!("Failed to parse UDS response: {}", e);
                (
                    StatusCode::BAD_GATEWAY,
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    json!({
                        "error": {
                            "message": format!("Invalid response from inference UDS: {}", e),
                            "type": "upstream_error",
                            "code": 502
                        }
                    })
                    .to_string(),
                )
                    .into_response()
            }
        },
        Err(e) => {
            error!("UDS inference error: {}", e);
            (
                StatusCode::BAD_GATEWAY,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                json!({
                    "error": {
                        "message": format!("UDS inference socket error: {}", e),
                        "type": "upstream_error",
                        "code": 502
                    }
                })
                .to_string(),
            )
                .into_response()
        }
    }
}

async fn anthropic_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<AnthropicRequest>,
) -> Response {
    if !check_auth(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json!({
                "type": "error",
                "error": {
                    "type": "authentication_error",
                    "message": "invalid x-api-key"
                }
            })
            .to_string(),
        )
            .into_response();
    }

    let model_req = payload.model.clone();
    let uds_req = convert_anthropic_to_uds(&payload, "gateway");
    let uds_bytes = match serde_json::to_vec(&uds_req) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                json!({ "type": "error", "error": { "type": "invalid_request_error", "message": e.to_string() } }).to_string(),
            ).into_response();
        }
    };

    let tool_count = payload.tools.as_ref().map(|t| t.len()).unwrap_or(0);
    let msg_count = payload.messages.len();
    info!(
        "Forwarding Anthropic request to UDS (VM: {}, req_id: {}, messages={}, tools={}, prompt_len={}, system_len={})",
        state.config.vm.id,
        uds_req.request_id,
        msg_count,
        tool_count,
        uds_req.prompt.len(),
        uds_req.system.as_ref().map(|s| s.len()).unwrap_or(0)
    );

    // Retry loop: handles ok:true + empty content (transient model bug)
    let max_empty_retries: u32 = 2;
    let mut uds_attempt: u32 = 0;
    let has_tools = payload.tools.is_some();

    let (final_uds_resp, raw_text) = loop {
        uds_attempt += 1;
        let req_id_label = if uds_attempt == 1 {
            uds_req.request_id.clone()
        } else {
            format!("{}-empty-retry-{}", uds_req.request_id, uds_attempt)
        };

        let call_bytes = if uds_attempt == 1 {
            uds_bytes.clone()
        } else {
            // Re-serialize with a fresh request_id for retry
            let mut retry_req = uds_req.clone();
            retry_req.request_id = req_id_label.clone();
            match serde_json::to_vec(&retry_req) {
                Ok(b) => b,
                Err(_) => uds_bytes.clone(),
            }
        };

        match state.uds_client.send_request(&call_bytes).await {
            Ok(resp_bytes) => {
                info!(
                    "Anthropic req_id={}: UDS response received ({} bytes), snippet={}",
                    req_id_label,
                    resp_bytes.len(),
                    String::from_utf8_lossy(&resp_bytes[..resp_bytes.len().min(500)])
                );
                match serde_json::from_slice::<UdsResponse>(&resp_bytes) {
                    Ok(uds_resp) => {
                        // Check if UDS returned an error
                        if !uds_resp.ok {
                            let err_msg = uds_resp
                                .error
                                .as_ref()
                                .and_then(|e| e.as_str())
                                .unwrap_or("inference_failed");
                            error!(
                                "Anthropic req_id={}: UDS returned ok=false, error={}",
                                req_id_label, err_msg
                            );
                            return (
                                StatusCode::UNPROCESSABLE_ENTITY,
                                [(axum::http::header::CONTENT_TYPE, "application/json")],
                                json!({
                                    "type": "error",
                                    "error": {
                                        "type": "api_error",
                                        "message": format!("Upstream inference error: {}", err_msg)
                                    }
                                })
                                .to_string(),
                            )
                                .into_response();
                        }

                        let (text, _, out_tokens, _) = extract_uds_content_text(&uds_resp);

                        // Retry if content is empty but model generated tokens (transient bug)
                        if text.trim().is_empty() && has_tools && uds_attempt <= max_empty_retries {
                            info!(
                                "Anthropic req_id={}: empty content with output_tokens={}, retrying ({}/{})",
                                req_id_label, out_tokens, uds_attempt, max_empty_retries
                            );
                            continue;
                        }

                        if text.trim().is_empty() && has_tools {
                            error!(
                                "Anthropic req_id={}: empty content after {} attempts, output_tokens={}, full_response={}",
                                req_id_label, uds_attempt, out_tokens,
                                String::from_utf8_lossy(&resp_bytes)
                            );
                            error!(
                                "Anthropic req_id={}: UDS prompt sent (first 3000 chars): {}",
                                req_id_label,
                                &uds_req.prompt[..uds_req.prompt.len().min(3000)]
                            );
                            error!(
                                "Anthropic req_id={}: UDS system sent (first 2000 chars): {}",
                                req_id_label,
                                uds_req
                                    .system
                                    .as_ref()
                                    .map(|s| &s[..s.len().min(2000)])
                                    .unwrap_or("")
                            );
                            return (
                                StatusCode::UNPROCESSABLE_ENTITY,
                                [(axum::http::header::CONTENT_TYPE, "application/json")],
                                json!({
                                    "type": "error",
                                    "error": {
                                        "type": "api_error",
                                        "message": "Model returned empty content for tool calling request"
                                    }
                                })
                                .to_string(),
                            )
                                .into_response();
                        }

                        break (uds_resp, text);
                    }
                    Err(e) => {
                        error!(
                            "Anthropic req_id={}: failed to parse UDS response: {}",
                            req_id_label, e
                        );
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            [(axum::http::header::CONTENT_TYPE, "application/json")],
                            json!({
                                "type": "error",
                                "error": {
                                    "type": "api_error",
                                    "message": format!("Failed to parse inference response: {}", e)
                                }
                            })
                            .to_string(),
                        )
                            .into_response();
                    }
                }
            }
            Err(e) => {
                error!("Anthropic req_id={}: UDS send failed: {}", req_id_label, e);
                return (
                    StatusCode::BAD_GATEWAY,
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    json!({
                        "type": "error",
                        "error": {
                            "type": "api_error",
                            "message": format!("Inference backend error: {}", e)
                        }
                    })
                    .to_string(),
                )
                    .into_response();
            }
        }
    };

    // --- Process the validated UDS response ---
    {
        let uds_resp = final_uds_resp;
        info!(
            "Anthropic req_id={}: raw_text length={}, has_tools={}, stream={}",
            uds_req.request_id,
            raw_text.len(),
            has_tools,
            payload.stream == Some(true)
        );
        let (mut is_tool, mut tool_calls, mut clean_text) = if payload.tools.is_some() {
            parse_tool_calls(&raw_text)
        } else {
            (false, Vec::new(), Some(raw_text))
        };
        info!(
            "Anthropic req_id={}: parsed tool_calls={}, is_tool={}, clean_text_len={}",
            uds_req.request_id,
            tool_calls.len(),
            is_tool,
            clean_text.as_ref().map(|t| t.len()).unwrap_or(0)
        );

        // Tool retry nudge loop if tools are required or forced
        let (tc_mode, forced_name) = normalize_tool_choice(payload.tool_choice.as_ref());
        let need_tool = tc_mode == "required" || tc_mode == "forced";

        let mut current_uds_resp = uds_resp;

        if payload.tools.is_some() && need_tool {
            let mut tool_satisfied = if tc_mode == "forced" && forced_name.is_some() {
                let fname = forced_name.as_deref().unwrap_or_default();
                is_tool && tool_calls.iter().any(|c| c.function.name == fname)
            } else {
                is_tool
            };

            let mut attempt = 1;
            let max_tool_retries = 3;

            while !tool_satisfied && attempt < max_tool_retries {
                attempt += 1;
                let nudge = "\n\nIMPORTANT: Your previous reply did not contain a tool call. You MUST respond with ONLY the JSON tool call object, nothing else.";
                let mut retry_req = uds_req.clone();
                let new_sys = match &retry_req.system {
                    Some(s) => format!("{}{}", s, nudge),
                    None => nudge.trim_start().to_string(),
                };
                retry_req.system = Some(new_sys);
                retry_req.request_id = format!("gw-retry-{}", uuid::Uuid::new_v4());

                if let Ok(retry_bytes) = serde_json::to_vec(&retry_req) {
                    if let Ok(retry_resp_bytes) = state.uds_client.send_request(&retry_bytes).await
                    {
                        if let Ok(retry_uds_resp) =
                            serde_json::from_slice::<UdsResponse>(&retry_resp_bytes)
                        {
                            let (retry_raw, _, _, _) = extract_uds_content_text(&retry_uds_resp);
                            let (r_is_tool, r_calls, r_clean) = parse_tool_calls(&retry_raw);
                            is_tool = r_is_tool;
                            tool_calls = r_calls;
                            clean_text = r_clean;
                            current_uds_resp = retry_uds_resp;

                            tool_satisfied = if tc_mode == "forced" && forced_name.is_some() {
                                let fname = forced_name.as_deref().unwrap_or_default();
                                is_tool && tool_calls.iter().any(|c| c.function.name == fname)
                            } else {
                                is_tool
                            };
                        }
                    }
                }
            }
        }

        if payload.stream == Some(true) {
            info!(
                "Anthropic req_id={}: sending SSE stream (is_tool={}, tool_calls={})",
                uds_req.request_id,
                is_tool,
                tool_calls.len()
            );
            let sse_str = format_uds_to_anthropic_stream(
                &current_uds_resp,
                model_req.as_deref(),
                is_tool,
                &tool_calls,
                clean_text.as_deref(),
            );
            info!(
                "Anthropic req_id={}: SSE payload ready ({} bytes)",
                uds_req.request_id,
                sse_str.len()
            );
            (
                StatusCode::OK,
                [
                    (
                        axum::http::header::CONTENT_TYPE.as_str(),
                        "text/event-stream; charset=utf-8",
                    ),
                    (
                        axum::http::header::CACHE_CONTROL.as_str(),
                        "no-cache, no-transform",
                    ),
                    (axum::http::header::CONNECTION.as_str(), "keep-alive"),
                    ("x-accel-buffering", "no"),
                ],
                sse_str,
            )
                .into_response()
        } else {
            info!(
                "Anthropic req_id={}: sending JSON response (is_tool={}, tool_calls={})",
                uds_req.request_id,
                is_tool,
                tool_calls.len()
            );
            let anthropic_resp = format_uds_to_anthropic(
                &current_uds_resp,
                model_req.as_deref(),
                is_tool,
                &tool_calls,
                clean_text.as_deref(),
            );
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(&anthropic_resp).unwrap_or_default(),
            )
                .into_response()
        }
    }
}

async fn openai_image_generations(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ImageGenerationRequest>,
) -> Response {
    if !check_auth(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json!({
                "error": {
                    "message": "Incorrect API key provided.",
                    "type": "invalid_request_error",
                    "code": "invalid_api_key"
                }
            })
            .to_string(),
        )
            .into_response();
    }

    if payload.prompt.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json!({
                "error": {
                    "message": "prompt is required",
                    "type": "invalid_request_error"
                }
            })
            .to_string(),
        )
            .into_response();
    }

    let size = payload.size.as_deref().unwrap_or("1024x1024");
    let orientation = match size {
        "1792x1024" => "landscape",
        "1024x1792" => "portrait",
        _ => "square",
    };

    let req_id = format!("gw-{}", uuid::Uuid::new_v4());
    let media_req = UdsMediaRequest {
        kind: "generate_image".to_string(),
        request_id: req_id.clone(),
        slug: "gateway".to_string(),
        prompt: payload.prompt.clone(),
        orientation: orientation.to_string(),
    };

    let req_bytes = match serde_json::to_vec(&media_req) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                json!({ "error": { "message": e.to_string(), "type": "invalid_request_error" } })
                    .to_string(),
            )
                .into_response();
        }
    };

    info!(
        "Forwarding image generation request to media UDS (req_id: {})",
        req_id
    );

    match state.media_client.send_request(&req_bytes).await {
        Ok(resp_bytes) => match serde_json::from_slice::<UdsMediaResponse>(&resp_bytes) {
            Ok(media_resp) => {
                if !media_resp.ok {
                    let err_msg = media_resp
                        .error
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "image generation failed".to_string());
                    return (
                        StatusCode::BAD_GATEWAY,
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        json!({ "error": { "message": err_msg, "type": "upstream_error" } })
                            .to_string(),
                    )
                        .into_response();
                }

                let result = media_resp.result.unwrap_or(crate::models::UdsMediaResult {
                    kind: None,
                    data_base64: None,
                    content_type: None,
                    ext: None,
                });

                let b64 = result.data_base64.unwrap_or_default();
                let created = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);

                (
                    StatusCode::OK,
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    json!({
                        "created": created,
                        "data": [
                            {
                                "b64_json": b64,
                                "revised_prompt": payload.prompt
                            }
                        ]
                    })
                    .to_string(),
                )
                    .into_response()
            }
            Err(e) => {
                error!("Failed to parse media UDS response: {}", e);
                (
                    StatusCode::BAD_GATEWAY,
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    json!({
                        "error": {
                            "message": format!("Invalid response from media UDS: {}", e),
                            "type": "upstream_error",
                            "code": 502
                        }
                    })
                    .to_string(),
                )
                    .into_response()
            }
        },
        Err(e) => {
            error!("Media UDS error: {}", e);
            (
                StatusCode::BAD_GATEWAY,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                json!({
                    "error": {
                        "message": format!("Media UDS socket error: {}", e),
                        "type": "upstream_error",
                        "code": 502
                    }
                })
                .to_string(),
            )
                .into_response()
        }
    }
}

#[derive(serde::Deserialize)]
pub struct SyncDownloadQuery {
    pub file: String,
}

async fn internal_sync_manifest(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !check_auth(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json!({ "error": "Unauthorized" }).to_string(),
        )
            .into_response();
    }

    let root_path = std::path::Path::new(&state.config.sync.sync_dir);
    match crate::sync::generate_manifest(root_path) {
        Ok(manifest) => {
            let mut headers = HeaderMap::new();
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::header::HeaderValue::from_static("application/json"),
            );
            if let Ok(val) = axum::http::header::HeaderValue::from_str(&manifest.version_hash) {
                headers.insert(
                    axum::http::header::HeaderName::from_static("x-cluster-version"),
                    val,
                );
            }
            (
                StatusCode::OK,
                headers,
                serde_json::to_string(&manifest).unwrap_or_default(),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json!({ "error": e.to_string() }).to_string(),
        )
            .into_response(),
    }
}

async fn internal_sync_download(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<SyncDownloadQuery>,
    req: axum::extract::Request,
) -> Response {
    if !check_auth(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json!({ "error": "Unauthorized" }).to_string(),
        )
            .into_response();
    }

    let root_path = std::path::Path::new(&state.config.sync.sync_dir);
    let target = match crate::sync::safe_resolve_sync(root_path, &query.file) {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                json!({ "error": "Invalid file path" }).to_string(),
            )
                .into_response();
        }
    };

    if !target.exists() || !target.is_file() {
        return (
            StatusCode::NOT_FOUND,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json!({ "error": "File not found" }).to_string(),
        )
            .into_response();
    }

    let serve = tower_http::services::ServeFile::new(&target);
    match tower::ServiceExt::oneshot(serve, req).await {
        Ok(res) => res.into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json!({ "error": e.to_string() }).to_string(),
        )
            .into_response(),
    }
}

pub async fn internal_sync_manifest_fm(
    State(state): State<crate::file_manager::FileManagerState>,
    headers: HeaderMap,
) -> Response {
    let auth_header = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let mut authenticated =
        crate::file_manager::auth::verify_basic_auth(auth_header, &state.config);

    if !authenticated {
        if let Some(ref expected_token) = state.config.auth_token {
            if !expected_token.is_empty() {
                if let Some(x_key) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
                    if x_key == expected_token {
                        authenticated = true;
                    }
                } else if let Some(auth_val) = auth_header {
                    if let Some(bearer) = auth_val.strip_prefix("Bearer ") {
                        if bearer == expected_token {
                            authenticated = true;
                        }
                    }
                }
            }
        }
    }

    if !authenticated {
        return (
            StatusCode::UNAUTHORIZED,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json!({ "error": "Unauthorized" }).to_string(),
        )
            .into_response();
    }

    let root_path = state.config.root_path();
    match crate::sync::generate_manifest(&root_path) {
        Ok(manifest) => {
            let mut headers = HeaderMap::new();
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::header::HeaderValue::from_static("application/json"),
            );
            if let Ok(val) = axum::http::header::HeaderValue::from_str(&manifest.version_hash) {
                headers.insert(
                    axum::http::header::HeaderName::from_static("x-cluster-version"),
                    val,
                );
            }
            (
                StatusCode::OK,
                headers,
                serde_json::to_string(&manifest).unwrap_or_default(),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json!({ "error": e.to_string() }).to_string(),
        )
            .into_response(),
    }
}

pub async fn internal_sync_download_fm(
    State(state): State<crate::file_manager::FileManagerState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<SyncDownloadQuery>,
    req: axum::extract::Request,
) -> Response {
    let auth_header = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let mut authenticated =
        crate::file_manager::auth::verify_basic_auth(auth_header, &state.config);

    if !authenticated {
        if let Some(ref expected_token) = state.config.auth_token {
            if !expected_token.is_empty() {
                if let Some(x_key) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
                    if x_key == expected_token {
                        authenticated = true;
                    }
                } else if let Some(auth_val) = auth_header {
                    if let Some(bearer) = auth_val.strip_prefix("Bearer ") {
                        if bearer == expected_token {
                            authenticated = true;
                        }
                    }
                }
            }
        }
    }

    if !authenticated {
        return (
            StatusCode::UNAUTHORIZED,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json!({ "error": "Unauthorized" }).to_string(),
        )
            .into_response();
    }

    let root_path = state.config.root_path();
    let target = match crate::sync::safe_resolve_sync(&root_path, &query.file) {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                json!({ "error": "Invalid file path" }).to_string(),
            )
                .into_response();
        }
    };

    if !target.exists() || !target.is_file() {
        return (
            StatusCode::NOT_FOUND,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json!({ "error": "File not found" }).to_string(),
        )
            .into_response();
    }

    let serve = tower_http::services::ServeFile::new(&target);
    match tower::ServiceExt::oneshot(serve, req).await {
        Ok(res) => res.into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json!({ "error": e.to_string() }).to_string(),
        )
            .into_response(),
    }
}
