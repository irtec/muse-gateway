use muse_agent::models::*;
use muse_agent::translator::*;
use serde_json::json;

#[test]
fn test_openai_tool_schema_and_prompt_conversion() {
    let req = ChatCompletionRequest {
        model: Some("gpt-4o".to_string()),
        messages: vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some(json!("You are an AI assistant.")),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(json!("What is the weather in Tokyo?")),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![json!({
                    "id": "call_123",
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "arguments": "{\"location\":\"Tokyo\"}"
                    }
                })]),
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some(json!(
                    "{\"temperature\": \"18C\", \"condition\": \"Sunny\"}"
                )),
                tool_calls: None,
                tool_call_id: Some("call_123".to_string()),
                name: Some("get_weather".to_string()),
            },
        ],
        stream: Some(false),
        tools: Some(vec![json!({
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get current weather in location",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "location": {"type": "string"}
                    },
                    "required": ["location"]
                }
            }
        })]),
        tool_choice: Some(json!("auto")),
        functions: None,
        function_call: None,
    };

    let uds_req = convert_openai_to_uds(&req, "gateway");

    assert!(uds_req.system.is_some());
    let sys = uds_req.system.unwrap();
    assert!(sys.contains("You have access to the following tools:"));
    assert!(sys.contains("get_weather"));
    assert!(sys.contains("Tool Calling Instructions:"));

    assert!(uds_req
        .prompt
        .contains("User: What is the weather in Tokyo?"));
    assert!(uds_req.prompt.contains(
        "Assistant: [Called tool 'get_weather' with arguments {\"location\":\"Tokyo\"}]"
    ));
    assert!(uds_req.prompt.contains(
        "Tool result (get_weather): {\"temperature\": \"18C\", \"condition\": \"Sunny\"}"
    ));
}

#[test]
fn test_parse_tool_calls_from_json_codeblock() {
    let model_output = r#"
I will check the weather for you.
```json
{
  "tool_calls": [
    {
      "name": "get_weather",
      "arguments": {
        "location": "Jakarta"
      }
    }
  ]
}
```
"#;

    let (is_tool, calls, clean_text) = parse_tool_calls(model_output);
    assert!(is_tool);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function.name, "get_weather");
    assert!(calls[0].function.arguments.contains("Jakarta"));
    assert_eq!(
        clean_text.as_deref(),
        Some("I will check the weather for you.")
    );
}

#[test]
fn test_parse_tool_calls_direct_dict() {
    let model_output = r#"{"name": "calculator", "arguments": {"expr": "15 * 4"}}"#;

    let (is_tool, calls, clean_text) = parse_tool_calls(model_output);
    assert!(is_tool);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function.name, "calculator");
    assert!(calls[0].function.arguments.contains("15 * 4"));
    assert!(clean_text.is_none() || clean_text == Some("".to_string()));
}

#[test]
fn test_openai_format_response_non_streaming() {
    let uds_resp = UdsResponse {
        ok: true,
        result: Some(UdsResult {
            kind: Some("complete".to_string()),
            content: Some(json!("Hello, how can I help you today?")),
            model: Some("muse".to_string()),
            usage: Some(UdsUsage {
                input_tokens: 15,
                output_tokens: 8,
                total_tokens: 23,
            }),
        }),
        error: None,
    };

    let val = format_uds_to_openai(
        &uds_resp,
        Some("gpt-4o"),
        false,
        &[],
        Some("Hello, how can I help you today?"),
    );
    assert_eq!(val["object"], "chat.completion");
    assert_eq!(val["model"], "gpt-4o");
    assert_eq!(val["choices"][0]["finish_reason"], "stop");
    assert_eq!(
        val["choices"][0]["message"]["content"],
        "Hello, how can I help you today?"
    );
    assert_eq!(val["usage"]["total_tokens"], 23);
}

#[test]
fn test_openai_format_response_streaming_tool_call() {
    let tool_call = ToolCall {
        id: "call_abc123".to_string(),
        call_type: "function".to_string(),
        function: ToolFunction {
            name: "search".to_string(),
            arguments: "{\"query\":\"rust axum\"}".to_string(),
        },
    };

    let uds_resp = UdsResponse {
        ok: true,
        result: Some(UdsResult {
            kind: Some("complete".to_string()),
            content: Some(json!("")),
            model: Some("muse".to_string()),
            usage: Some(UdsUsage {
                input_tokens: 20,
                output_tokens: 10,
                total_tokens: 30,
            }),
        }),
        error: None,
    };

    let sse_output =
        format_uds_to_openai_stream(&uds_resp, Some("gpt-4o"), true, &[tool_call], None);

    assert!(sse_output.contains("data: "));
    assert!(sse_output.contains("call_abc123"));
    assert!(sse_output.contains("search"));
    assert!(sse_output.contains(r#"\"query\":\"rust axum\""#));
    assert!(sse_output.contains("\"finish_reason\":\"tool_calls\""));
    assert!(sse_output.ends_with("data: [DONE]\n\n"));
}

#[test]
fn test_anthropic_format_and_stream() {
    let tool_call = ToolCall {
        id: "toolu_abc123".to_string(),
        call_type: "function".to_string(),
        function: ToolFunction {
            name: "fetch_page".to_string(),
            arguments: "{\"url\":\"https://example.com\"}".to_string(),
        },
    };

    let uds_resp = UdsResponse {
        ok: true,
        result: Some(UdsResult {
            kind: Some("complete".to_string()),
            content: Some(json!("")),
            model: Some("muse".to_string()),
            usage: Some(UdsUsage {
                input_tokens: 45,
                output_tokens: 25,
                total_tokens: 70,
            }),
        }),
        error: None,
    };

    // 1. Non-streaming Anthropic
    let val = format_uds_to_anthropic(
        &uds_resp,
        Some("claude-3-5-sonnet-20241022"),
        true,
        std::slice::from_ref(&tool_call),
        Some("Let me fetch that:"),
    );

    assert_eq!(val["type"], "message");
    assert_eq!(val["stop_reason"], "tool_use");
    let content = val["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "Let me fetch that:");
    assert_eq!(content[1]["type"], "tool_use");
    assert_eq!(content[1]["name"], "fetch_page");
    assert_eq!(content[1]["input"]["url"], "https://example.com");

    // 2. Streaming Anthropic
    let sse_output = format_uds_to_anthropic_stream(
        &uds_resp,
        Some("claude-3-5-sonnet-20241022"),
        true,
        &[tool_call],
        Some("Let me fetch that:"),
    );

    assert!(sse_output.contains("event: message_start"));
    assert!(sse_output.contains("event: content_block_start"));
    assert!(sse_output.contains("event: content_block_delta"));
    assert!(sse_output.contains("input_json_delta"));
    assert!(sse_output.contains("event: message_delta"));
    assert!(sse_output.contains("\"stop_reason\":\"tool_use\""));
    assert!(sse_output.contains("event: message_stop"));
}

#[test]
fn test_anthropic_convert_parallel_tool_rounds() {
    let req = AnthropicRequest {
        model: Some("claude-3-5-sonnet-20241022".to_string()),
        system: Some(json!("You are a helpful assistant.")),
        messages: vec![
            AnthropicMessage {
                role: "user".to_string(),
                content: json!("Check the weather in Paris and Tokyo."),
            },
            AnthropicMessage {
                role: "assistant".to_string(),
                content: json!([
                    {
                        "type": "text",
                        "text": "I'll check both cities for you."
                    },
                    {
                        "type": "tool_use",
                        "id": "toolu_paris_1",
                        "name": "get_weather",
                        "input": { "city": "Paris" }
                    },
                    {
                        "type": "tool_use",
                        "id": "toolu_tokyo_2",
                        "name": "get_weather",
                        "input": { "city": "Tokyo" }
                    }
                ]),
            },
            AnthropicMessage {
                role: "user".to_string(),
                content: json!([
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_paris_1",
                        "content": "18°C, sunny"
                    },
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_tokyo_2",
                        "content": "22°C, clear"
                    }
                ]),
            },
        ],
        stream: None,
        tools: None,
        tool_choice: None,
    };

    let uds_req = convert_anthropic_to_uds(&req, "muse");

    // Assistant text should precede tool calls
    assert!(uds_req
        .prompt
        .contains("Assistant: I'll check both cities for you."));
    // Each tool_use must emit an Assistant line, kept together
    assert!(uds_req
        .prompt
        .contains("Assistant: [Called tool 'get_weather' with arguments {\"city\":\"Paris\"}]"));
    assert!(uds_req
        .prompt
        .contains("Assistant: [Called tool 'get_weather' with arguments {\"city\":\"Tokyo\"}]"));
    // Both tool results must be present in prompt
    assert!(uds_req
        .prompt
        .contains("Tool result (toolu_paris_1): 18°C, sunny"));
    assert!(uds_req
        .prompt
        .contains("Tool result (toolu_tokyo_2): 22°C, clear"));
}

#[test]
fn test_anthropic_convert_sequential_tool_results() {
    let req = AnthropicRequest {
        model: Some("claude-3-5-sonnet-20241022".to_string()),
        system: None,
        messages: vec![
            AnthropicMessage {
                role: "assistant".to_string(),
                content: json!([
                    {
                        "type": "tool_use",
                        "id": "toolu_valid_1",
                        "name": "read_file",
                        "input": { "path": "/etc/hosts" }
                    }
                ]),
            },
            AnthropicMessage {
                role: "user".to_string(),
                content: json!([
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_valid_1",
                        "content": "127.0.0.1 localhost"
                    },
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_extra_2",
                        "content": "extra content"
                    }
                ]),
            },
        ],
        stream: None,
        tools: None,
        tool_choice: None,
    };

    let uds_req = convert_anthropic_to_uds(&req, "muse");

    // All emitted tool results are preserved sequentially
    assert!(uds_req
        .prompt
        .contains("Tool result (toolu_valid_1): 127.0.0.1 localhost"));
    assert!(uds_req
        .prompt
        .contains("Tool result (toolu_extra_2): extra content"));
}

#[test]
fn test_anthropic_convert_tool_result_with_image_content() {
    let req = AnthropicRequest {
        model: Some("claude-3-5-sonnet-20241022".to_string()),
        system: None,
        messages: vec![
            AnthropicMessage {
                role: "user".to_string(),
                content: json!("Generate a blue robot"),
            },
            AnthropicMessage {
                role: "assistant".to_string(),
                content: json!([
                    {
                        "type": "tool_use",
                        "id": "toolu_img_1",
                        "name": "generate_image",
                        "input": { "prompt": "a friendly blue robot" }
                    }
                ]),
            },
            AnthropicMessage {
                role: "user".to_string(),
                content: json!([
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_img_1",
                        "content": [
                            {
                                "type": "text",
                                "text": "Generated image successfully"
                            },
                            {
                                "type": "image",
                                "source": {
                                    "type": "base64",
                                    "media_type": "image/png",
                                    "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
                                }
                            }
                        ]
                    }
                ]),
            },
        ],
        stream: None,
        tools: None,
        tool_choice: None,
    };

    let uds_req = convert_anthropic_to_uds(&req, "muse");

    // The image must be extracted to the images payload (for UDS media)
    assert!(uds_req.images.is_some());
    let images = uds_req.images.unwrap();
    assert_eq!(images.len(), 1);
    let img_b64 = images[0]
        .get("data_base64")
        .and_then(|v| v.as_str())
        .unwrap();
    assert!(img_b64.starts_with("data:image/png;base64,iVBORw"));

    // The prompt must NOT contain the huge raw base64 string, but a clean summary
    assert!(uds_req.prompt.contains(
        "Tool result (toolu_img_1): Generated image successfully [Image generated and attached]"
    ));
    assert!(!uds_req.prompt.contains("iVBORw0KGgoAAA"));
}

#[test]
fn test_openai_convert_multiple_tool_messages_coalesced() {
    let req = ChatCompletionRequest {
        model: Some("gpt-4o".to_string()),
        messages: vec![
            ChatMessage {
                role: "user".to_string(),
                content: Some(json!("Call tools A and B")),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![
                    json!({
                        "id": "call_a",
                        "type": "function",
                        "function": {
                            "name": "tool_a",
                            "arguments": "{\"x\": 1}"
                        }
                    }),
                    json!({
                        "id": "call_b",
                        "type": "function",
                        "function": {
                            "name": "tool_b",
                            "arguments": "{\"y\": 2}"
                        }
                    }),
                ]),
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some(json!("result A")),
                tool_calls: None,
                tool_call_id: Some("call_a".to_string()),
                name: Some("tool_a".to_string()),
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some(json!("result B")),
                tool_calls: None,
                tool_call_id: Some("call_b".to_string()),
                name: Some("tool_b".to_string()),
            },
        ],
        stream: Some(false),
        tools: None,
        tool_choice: None,
        functions: None,
        function_call: None,
    };

    let uds_req = convert_openai_to_uds(&req, "gateway");

    // Both results must be present
    assert!(uds_req.prompt.contains("Tool result (tool_a): result A"));
    assert!(uds_req.prompt.contains("Tool result (tool_b): result B"));
}

#[test]
fn test_parse_tool_calls_no_deduplication_repeated_calls() {
    let content = r#"{"tool_calls": [
        {"name": "lookup_memories", "arguments": {"query": "test1"}},
        {"name": "lookup_memories", "arguments": {"query": "test2"}},
        {"name": "lookup_memories", "arguments": {"query": "test1"}}
    ]}"#;

    let (is_tool, calls, _) = parse_tool_calls(content);
    assert!(is_tool);
    // All 3 calls must be preserved even if name and arguments repeat
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].function.name, "lookup_memories");
    assert_eq!(calls[1].function.name, "lookup_memories");
    assert_eq!(calls[2].function.name, "lookup_memories");

    // All IDs must be prefixed with toolu_ for Anthropic compatibility
    for c in &calls {
        assert!(c.id.starts_with("toolu_"));
    }
}

#[test]
fn test_format_uds_to_anthropic_non_stream_tool_ids_and_args() {
    let uds_resp = UdsResponse {
        ok: true,
        result: Some(UdsResult {
            kind: Some("complete".to_string()),
            model: Some("muse".to_string()),
            content: Some(json!("")),
            usage: Some(UdsUsage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30,
            }),
        }),
        error: None,
    };

    let tool_calls = vec![
        ToolCall {
            id: "call_abc123".to_string(),
            call_type: "function".to_string(),
            function: ToolFunction {
                name: "lookup_memories".to_string(),
                arguments: "{\"query\":\"test\"}".to_string(),
            },
        },
        ToolCall {
            id: "".to_string(),
            call_type: "function".to_string(),
            function: ToolFunction {
                name: "get_weather".to_string(),
                arguments: "invalid-json".to_string(),
            },
        },
    ];

    let val = format_uds_to_anthropic(
        &uds_resp,
        Some("claude-3-5-sonnet"),
        true,
        &tool_calls,
        None,
    );

    assert_eq!(val["stop_reason"], "tool_use");
    let content = val["content"]
        .as_array()
        .expect("content should be an array");
    assert_eq!(content.len(), 2);

    // Call 1: call_ prefix converted to toolu_
    assert_eq!(content[0]["id"], "toolu_abc123");
    assert_eq!(content[0]["name"], "lookup_memories");
    assert_eq!(content[0]["input"]["query"], "test");

    // Call 2: empty id gets toolu_ uuid, invalid json falls back to empty object {}
    assert!(content[1]["id"].as_str().unwrap().starts_with("toolu_"));
    assert_eq!(content[1]["name"], "get_weather");
    assert!(content[1]["input"].is_object());
    assert_eq!(content[1]["input"].as_object().unwrap().len(), 0);
}
