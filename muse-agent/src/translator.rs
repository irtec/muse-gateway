use serde_json::{json, Value};
use tracing::info;
use uuid::Uuid;

use crate::models::{
    AnthropicRequest, ChatCompletionRequest, ToolCall, ToolFunction, UdsInferenceRequest,
    UdsResponse,
};

pub fn extract_content_text(val: &Value) -> String {
    match val {
        Value::String(s) => {
            if s.starts_with("data:image/")
                || (s.len() > 1000
                    && s.chars().all(|c| {
                        c.is_ascii_alphanumeric()
                            || c == '+'
                            || c == '/'
                            || c == '='
                            || c.is_whitespace()
                    }))
            {
                "[Image data]".to_string()
            } else {
                s.clone()
            }
        }
        Value::Array(arr) => {
            let mut parts = Vec::new();
            for item in arr {
                if let Some(txt) = item.get("text").and_then(|t| t.as_str()) {
                    parts.push(txt.to_string());
                } else if let Some(s) = item.as_str() {
                    if s.starts_with("data:image/") {
                        parts.push("[Image data]".to_string());
                    } else {
                        parts.push(s.to_string());
                    }
                } else if let Some(txt) = item.get("content").and_then(|t| t.as_str()) {
                    parts.push(txt.to_string());
                } else if item.get("type").and_then(|v| v.as_str()) == Some("image") {
                    parts.push("[Image attached]".to_string());
                }
            }
            parts.join(" ")
        }
        Value::Null => String::new(),
        _ => val.to_string(),
    }
}

pub fn extract_tool_result_content(val: &Value, images_out: &mut Vec<Value>) -> String {
    match val {
        Value::String(s) => {
            if s.starts_with("data:image/") {
                if let Some(img) = normalize_image_data(val) {
                    images_out.push(img);
                }
                "[Image generated and attached]".to_string()
            } else {
                s.clone()
            }
        }
        Value::Array(arr) => {
            let mut text_parts = Vec::new();
            for block in arr {
                if let Value::Object(map) = block {
                    let btype = map.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if btype == "image" {
                        if let Some(img) = normalize_image_data(block) {
                            images_out.push(img);
                        }
                        text_parts.push("[Image generated and attached]".to_string());
                    } else if btype == "text" {
                        if let Some(txt) = map.get("text").and_then(|v| v.as_str()) {
                            let trimmed = txt.trim();
                            if !trimmed.is_empty() {
                                text_parts.push(trimmed.to_string());
                            }
                        }
                    } else if let Some(txt) = map.get("content").and_then(|v| v.as_str()) {
                        let trimmed = txt.trim();
                        if !trimmed.is_empty() {
                            text_parts.push(trimmed.to_string());
                        }
                    }
                } else if let Some(s) = block.as_str() {
                    if s.starts_with("data:image/") {
                        if let Some(img) = normalize_image_data(block) {
                            images_out.push(img);
                        }
                        text_parts.push("[Image generated and attached]".to_string());
                    } else {
                        text_parts.push(s.to_string());
                    }
                }
            }
            if text_parts.is_empty() {
                "[Tool execution completed]".to_string()
            } else {
                text_parts.join(" ")
            }
        }
        Value::Object(map) => {
            if map.get("type").and_then(|v| v.as_str()) == Some("image") {
                if let Some(img) = normalize_image_data(val) {
                    images_out.push(img);
                }
                "[Image generated and attached]".to_string()
            } else {
                extract_content_text(val)
            }
        }
        Value::Null => String::new(),
        _ => val.to_string(),
    }
}

fn sanitize_image_data_uri(data_or_uri: &str, media_type: Option<&str>) -> String {
    let (data_part, mime) = if let Some(stripped) = data_or_uri.strip_prefix("data:") {
        if let Some(comma_pos) = stripped.find(',') {
            let meta = &stripped[..comma_pos];
            let raw_data = &stripped[comma_pos + 1..];
            let meta_mime = meta.split(';').next().unwrap_or("image/png");
            (raw_data, meta_mime)
        } else {
            (data_or_uri, "image/png")
        }
    } else {
        (data_or_uri, media_type.unwrap_or("image/png"))
    };

    // space-inference.sock strictly accepts image/jpeg and image/png.
    // WebP and others trigger "Web artifact inference failed." error.
    let safe_mime = match mime.to_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => "image/jpeg",
        _ => "image/png",
    };

    format!("data:{};base64,{}", safe_mime, data_part)
}

pub fn normalize_image_data(val: &Value) -> Option<Value> {
    if let Value::String(s) = val {
        return Some(json!({ "data_base64": sanitize_image_data_uri(s, None) }));
    }

    if let Value::Object(map) = val {
        // Anthropic format: { "type": "image", "source": { "type": "base64", "media_type": "...", "data": "..." } }
        if map.get("type").and_then(|v| v.as_str()) == Some("image") {
            if let Some(src) = map.get("source").and_then(|v| v.as_object()) {
                let data = src.get("data").and_then(|v| v.as_str()).unwrap_or("");
                let media_type = src
                    .get("media_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("image/png");
                if !data.is_empty() {
                    return Some(
                        json!({ "data_base64": sanitize_image_data_uri(data, Some(media_type)) }),
                    );
                }
            }
        }

        // OpenAI format: { "type": "image_url", "image_url": { "url": "..." } }
        if map.get("type").and_then(|v| v.as_str()) == Some("image_url") {
            if let Some(img_url) = map.get("image_url").and_then(|v| v.as_object()) {
                if let Some(url) = img_url.get("url").and_then(|v| v.as_str()) {
                    return Some(json!({ "data_base64": sanitize_image_data_uri(url, None) }));
                }
            }
        }
    }

    None
}

pub fn normalize_tools(tools: Option<&[Value]>) -> Vec<Value> {
    let mut out = Vec::new();
    let items = match tools {
        Some(t) => t,
        None => return out,
    };

    for item in items {
        if let Value::Object(map) = item {
            if let Some(fn_obj) = map.get("function").and_then(|v| v.as_object()) {
                out.push(json!({
                    "name": fn_obj.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    "description": fn_obj.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                    "parameters": fn_obj.get("parameters").cloned().unwrap_or(json!({})),
                }));
            } else {
                let params = map
                    .get("input_schema")
                    .or_else(|| map.get("parameters"))
                    .cloned()
                    .unwrap_or(json!({}));
                out.push(json!({
                    "name": map.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    "description": map.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                    "parameters": params,
                }));
            }
        }
    }
    out
}

pub fn normalize_tool_choice(tool_choice: Option<&Value>) -> (String, Option<String>) {
    let tc = match tool_choice {
        Some(v) => v,
        None => return ("auto".to_string(), None),
    };

    if let Value::String(s) = tc {
        let val = s.trim().to_lowercase();
        if val == "none" {
            return ("none".to_string(), None);
        }
        if val == "required" || val == "any" {
            return ("required".to_string(), None);
        }
        return ("auto".to_string(), None);
    }

    if let Value::Object(map) = tc {
        let ttype = map
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        if ttype == "none" {
            return ("none".to_string(), None);
        }
        if ttype == "required" || ttype == "any" {
            return ("required".to_string(), None);
        }
        if ttype == "function" || ttype == "tool" {
            let mut name = None;
            if let Some(fn_obj) = map.get("function").and_then(|v| v.as_object()) {
                name = fn_obj
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }
            if name.is_none() {
                name = map
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }
            if let Some(n) = name {
                return ("forced".to_string(), Some(n));
            } else {
                return ("required".to_string(), None);
            }
        }
    }

    ("auto".to_string(), None)
}

pub fn json_candidates(text: &str) -> Vec<String> {
    let mut candidates = Vec::new();

    // 1. Code blocks: ```json ... ``` or ``` ... ```
    let mut start_idx = 0;
    while let Some(pos) = text[start_idx..].find("```") {
        let actual_start = start_idx + pos;
        let content_start = actual_start + 3;
        if let Some(end_pos) = text[content_start..].find("```") {
            let actual_end = content_start + end_pos;
            let block = &text[content_start..actual_end];
            let clean = if block.trim_start().starts_with("json") {
                block.trim_start()[4..].trim()
            } else {
                block.trim()
            };
            if !clean.is_empty() {
                candidates.push(clean.to_string());
            }
            start_idx = actual_end + 3;
        } else {
            break;
        }
    }

    // 2. Balanced { ... } and [ ... ]
    let bytes = text.as_bytes();
    let mut stack: Vec<u8> = Vec::new();
    let mut start: Option<usize> = None;
    let mut in_str = false;
    let mut esc = false;

    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }

        if b == b'"' {
            in_str = true;
            continue;
        }

        if b == b'{' || b == b'[' {
            if stack.is_empty() {
                start = Some(i);
            }
            let closing = if b == b'{' { b'}' } else { b']' };
            stack.push(closing);
        } else if (b == b'}' || b == b']') && !stack.is_empty() {
            if Some(&b) == stack.last() {
                stack.pop();
                if stack.is_empty() {
                    if let Some(s_idx) = start {
                        let cand = &text[s_idx..=i];
                        if !candidates.contains(&cand.to_string()) {
                            candidates.push(cand.to_string());
                        }
                        start = None;
                    }
                }
            } else {
                stack.clear();
                start = None;
            }
        }
    }

    candidates
}

fn call_obj(name: &str, args: &Value) -> ToolCall {
    let args_str = match args {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.starts_with('{') && trimmed.ends_with('}') {
                trimmed.to_string()
            } else if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
                if val.is_object() {
                    trimmed.to_string()
                } else {
                    json!({ "value": val }).to_string()
                }
            } else {
                json!({ "input": s }).to_string()
            }
        }
        Value::Object(_) => serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string()),
        _ => json!({ "value": args }).to_string(),
    };

    let id = format!("toolu_{}", Uuid::new_v4().simple());
    ToolCall {
        id,
        call_type: "function".to_string(),
        function: ToolFunction {
            name: name.to_string(),
            arguments: args_str,
        },
    }
}

fn calls_from_item(item: &Value) -> Option<Vec<ToolCall>> {
    let map = item.as_object()?;

    // Case A: { "tool_calls": [...] } or { "tools": [...] }
    for key in ["tool_calls", "tools"] {
        if let Some(arr) = map.get(key).and_then(|v| v.as_array()) {
            let mut calls = Vec::new();
            for sub in arr {
                if let Some(sub_calls) = calls_from_item(sub) {
                    calls.extend(sub_calls);
                }
            }
            if !calls.is_empty() {
                return Some(calls);
            }
        }
    }

    // Case B: { "name": X, "arguments" | "parameters" | "input" | "args": {...} }
    if let Some(name) = map.get("name").and_then(|v| v.as_str()) {
        for akey in ["arguments", "parameters", "input", "args"] {
            if let Some(args_val) = map.get(akey) {
                return Some(vec![call_obj(name, args_val)]);
            }
        }
        // Name without arguments:
        let valid_keys = ["name", "type", "id"];
        if map.keys().all(|k| valid_keys.contains(&k.as_str())) {
            return Some(vec![call_obj(name, &json!({}))]);
        }
    }

    None
}

pub fn parse_tool_calls(content: &str) -> (bool, Vec<ToolCall>, Option<String>) {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return (false, Vec::new(), Some(String::new()));
    }

    let candidates = json_candidates(trimmed);
    let mut calls = Vec::new();
    let mut first_cand_str: Option<String> = None;

    for cand in &candidates {
        let parsed: Value = match serde_json::from_str(cand) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let items: Vec<Value> = match parsed {
            Value::Array(arr) => arr,
            Value::Object(_) => vec![parsed],
            _ => continue,
        };

        let mut found_here = Vec::new();
        for item in items {
            if let Some(item_calls) = calls_from_item(&item) {
                found_here.extend(item_calls);
            }
        }

        for call in found_here {
            calls.push(call);
        }

        if !calls.is_empty() && first_cand_str.is_none() {
            first_cand_str = Some(cand.clone());
        }
    }

    if !calls.is_empty() {
        let clean_text = if let Some(first_cand) = first_cand_str {
            if let Some(idx) = trimmed.find(&first_cand) {
                let mut prefix = trimmed[..idx].trim();
                if let Some(stripped) = prefix.strip_suffix("```json") {
                    prefix = stripped.trim();
                } else if let Some(stripped) = prefix.strip_suffix("```") {
                    prefix = stripped.trim();
                }
                if prefix.is_empty() {
                    None
                } else {
                    Some(prefix.to_string())
                }
            } else {
                None
            }
        } else {
            None
        };
        (true, calls, clean_text)
    } else {
        (false, Vec::new(), Some(content.to_string()))
    }
}

pub fn convert_openai_to_uds(
    req: &ChatCompletionRequest,
    default_slug: &str,
) -> UdsInferenceRequest {
    let mut system_parts = Vec::new();
    let mut turns = Vec::new();
    let mut images = Vec::new();

    let tools_raw = req.tools.as_deref().or(req.functions.as_deref());
    let tool_choice_raw = req.tool_choice.as_ref().or(req.function_call.as_ref());

    for msg in &req.messages {
        let role = msg.role.trim().to_lowercase();
        let raw_content = &msg.content;

        // 1. System / developer message
        if role == "system" || role == "developer" {
            if let Some(c) = raw_content {
                let text = extract_content_text(c);
                if !text.is_empty() {
                    system_parts.push(text);
                }
            }
            continue;
        }

        let cap_role = if role.is_empty() {
            "User".to_string()
        } else {
            let mut c = role.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        };

        // 2. Multimodal blocks
        if let Some(Value::Array(arr)) = raw_content {
            let mut text_parts = Vec::new();
            for block in arr {
                if let Value::Object(bmap) = block {
                    let btype = bmap.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match btype {
                        "text" => {
                            if let Some(t) = bmap.get("text").and_then(|v| v.as_str()) {
                                let trimmed = t.trim();
                                if !trimmed.is_empty() {
                                    text_parts.push(trimmed.to_string());
                                }
                            }
                        }
                        "image_url" | "image" => {
                            if let Some(img) = normalize_image_data(block) {
                                images.push(img);
                            }
                        }
                        "tool_use" => {
                            let tname = bmap.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            let tinput = bmap.get("input").cloned().unwrap_or(json!({}));
                            let arg_str =
                                serde_json::to_string(&tinput).unwrap_or_else(|_| "{}".to_string());
                            turns.push(format!(
                                "Assistant: [Called tool '{}' with arguments {}]",
                                tname, arg_str
                            ));
                        }
                        "tool_result" => {
                            let tid = bmap
                                .get("tool_use_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("tool");
                            let tcontent = bmap.get("content").cloned().unwrap_or(json!(""));
                            let c_text = extract_tool_result_content(&tcontent, &mut images);
                            turns.push(format!("Tool result ({}): {}", tid, c_text));
                        }
                        _ => {
                            if let Some(t) = bmap.get("text").and_then(|v| v.as_str()) {
                                let trimmed = t.trim();
                                if !trimmed.is_empty() {
                                    text_parts.push(trimmed.to_string());
                                }
                            }
                        }
                    }
                } else if let Some(s) = block.as_str() {
                    let trimmed = s.trim();
                    if !trimmed.is_empty() {
                        text_parts.push(trimmed.to_string());
                    }
                }
            }

            if text_parts.is_empty() && !images.is_empty() && role == "user" && turns.is_empty() {
                turns.push("User: Describe the attached image.".to_string());
            } else if !text_parts.is_empty() {
                turns.push(format!("{}: {}", cap_role, text_parts.join(" ")));
            }
            continue;
        }

        // 3. Standard string or tool structure
        let text = raw_content
            .as_ref()
            .map(extract_content_text)
            .unwrap_or_default();
        let trimmed_text = text.trim();

        if role == "assistant" {
            let mut parts = Vec::new();
            if !trimmed_text.is_empty() {
                parts.push(trimmed_text.to_string());
            }
            if let Some(tool_calls) = &msg.tool_calls {
                for tc in tool_calls {
                    if let Some(tc_map) = tc.as_object() {
                        let fn_obj = tc_map.get("function").and_then(|v| v.as_object());
                        let fn_name = fn_obj
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let args = fn_obj
                            .and_then(|f| f.get("arguments"))
                            .map(|v| match v {
                                Value::String(s) => s.clone(),
                                _ => serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()),
                            })
                            .unwrap_or_else(|| "{}".to_string());
                        parts.push(format!(
                            "[Called tool '{}' with arguments {}]",
                            fn_name, args
                        ));
                    }
                }
            }
            if !parts.is_empty() {
                turns.push(format!("Assistant: {}", parts.join("\n")));
            }
        } else if role == "tool" || role == "function" {
            let tag = msg
                .name
                .as_deref()
                .or(msg.tool_call_id.as_deref())
                .unwrap_or("tool");
            turns.push(format!("Tool result ({}): {}", tag, trimmed_text));
        } else if role == "user" {
            if !trimmed_text.is_empty() {
                turns.push(format!("User: {}", trimmed_text));
            } else if !images.is_empty() && turns.is_empty() {
                turns.push("User: Describe the attached image.".to_string());
            }
        } else if !trimmed_text.is_empty() {
            turns.push(format!("{}: {}", cap_role, trimmed_text));
        }
    }

    // 4. Inject tool schemas into system instructions
    let norm_tools = normalize_tools(tools_raw);
    let (tc_mode, tc_name) = normalize_tool_choice(tool_choice_raw);
    if !norm_tools.is_empty() && tc_mode != "none" {
        let tools_json = serde_json::to_string(&norm_tools).unwrap_or_default();
        let mut tool_instructions = vec![
            "You have access to the following tools:".to_string(),
            tools_json,
            "\nTool Calling Instructions:".to_string(),
            "- When you need to call a tool, respond with a JSON markdown code block in this format:"
                .to_string(),
            "```json".to_string(),
            r#"{"name": "<tool_name>", "arguments": {<arguments>}}"#.to_string(),
            "```".to_string(),
            "- Or if calling multiple tools, provide a JSON array:".to_string(),
            "```json".to_string(),
            r#"[{"name": "<tool_name>", "arguments": {<arguments>}}]"#.to_string(),
            "```".to_string(),
        ];
        if tc_mode == "forced" {
            let n = tc_name.as_deref().unwrap_or("tool");
            tool_instructions.push(format!("- MANDATORY: You MUST call the tool '{}'.", n));
            tool_instructions.push(
                "- Do not reply with plain text; always emit the JSON code block.".to_string(),
            );
        } else if tc_mode == "required" {
            tool_instructions.push("- MANDATORY: You MUST call at least one tool.".to_string());
            tool_instructions.push(
                "- Do not reply with plain text; always emit the JSON code block.".to_string(),
            );
        } else {
            tool_instructions.push("- If no tool is needed or appropriate, reply with conversational text without JSON.".to_string());
        }
        system_parts.push(tool_instructions.join("\n"));
    }

    let prompt = turns.join("\n\n");

    let system = if !system_parts.is_empty() {
        Some(system_parts.join("\n\n"))
    } else {
        None
    };

    let images_opt = if images.is_empty() {
        None
    } else {
        Some(images)
    };

    UdsInferenceRequest {
        kind: "complete".to_string(),
        request_id: format!("gw-{}", Uuid::new_v4()),
        slug: default_slug.to_string(),
        prompt,
        timeout_secs: 180,
        system,
        images: images_opt,
    }
}

pub fn convert_anthropic_to_uds(req: &AnthropicRequest, default_slug: &str) -> UdsInferenceRequest {
    let mut system_parts = Vec::new();
    let mut turns = Vec::new();
    let mut images = Vec::new();

    if let Some(sys) = &req.system {
        let text = extract_content_text(sys);
        if !text.is_empty() {
            system_parts.push(text);
        }
    }

    for msg in &req.messages {
        let role = msg.role.trim().to_lowercase();
        if role == "system" {
            let text = extract_content_text(&msg.content);
            if !text.is_empty() {
                system_parts.push(text);
            }
            continue;
        }

        let cap_role = if role.is_empty() {
            "User".to_string()
        } else {
            let mut c = role.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        };

        match &msg.content {
            Value::Array(arr) => {
                let mut text_parts = Vec::new();
                for block in arr {
                    if let Value::Object(bmap) = block {
                        let btype = bmap.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        match btype {
                            "text" => {
                                if let Some(t) = bmap.get("text").and_then(|v| v.as_str()) {
                                    let trimmed = t.trim();
                                    if !trimmed.is_empty() {
                                        text_parts.push(trimmed.to_string());
                                    }
                                }
                            }
                            "image" => {
                                if let Some(img) = normalize_image_data(block) {
                                    images.push(img);
                                }
                            }
                            "tool_use" => {
                                let tname = bmap.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                let tinput = bmap.get("input").cloned().unwrap_or(json!({}));
                                let arg_str = serde_json::to_string(&tinput)
                                    .unwrap_or_else(|_| "{}".to_string());
                                turns.push(format!(
                                    "Assistant: [Called tool '{}' with arguments {}]",
                                    tname, arg_str
                                ));
                            }
                            "tool_result" => {
                                let tid = bmap
                                    .get("tool_use_id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("tool");
                                let tcontent = bmap.get("content").cloned().unwrap_or(json!(""));
                                let c_text = extract_tool_result_content(&tcontent, &mut images);
                                turns.push(format!("Tool result ({}): {}", tid, c_text));
                            }
                            _ => {
                                if let Some(t) = bmap.get("text").and_then(|v| v.as_str()) {
                                    let trimmed = t.trim();
                                    if !trimmed.is_empty() {
                                        text_parts.push(trimmed.to_string());
                                    }
                                }
                            }
                        }
                    } else if let Some(s) = block.as_str() {
                        let trimmed = s.trim();
                        if !trimmed.is_empty() {
                            text_parts.push(trimmed.to_string());
                        }
                    }
                }

                if !text_parts.is_empty() {
                    turns.push(format!("{}: {}", cap_role, text_parts.join(" ")));
                } else if !images.is_empty() && role == "user" && turns.is_empty() {
                    turns.push("User: Describe the attached image.".to_string());
                }
            }
            Value::String(s) => {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    turns.push(format!("{}: {}", cap_role, trimmed));
                }
            }
            _ => {
                let text = extract_content_text(&msg.content);
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    turns.push(format!("{}: {}", cap_role, trimmed));
                }
            }
        }
    }

    // Inject tool schemas
    let norm_tools = normalize_tools(req.tools.as_deref());
    let (tc_mode, tc_name) = normalize_tool_choice(req.tool_choice.as_ref());
    if !norm_tools.is_empty() && tc_mode != "none" {
        let tools_json = serde_json::to_string(&norm_tools).unwrap_or_default();
        let mut tool_instructions = vec![
            "You have access to the following tools:".to_string(),
            tools_json,
            "\nTool Calling Instructions:".to_string(),
            "- When you need to call a tool, respond with a JSON markdown code block in this format:"
                .to_string(),
            "```json".to_string(),
            r#"{"name": "<tool_name>", "arguments": {<arguments>}}"#.to_string(),
            "```".to_string(),
            "- Or if calling multiple tools, provide a JSON array:".to_string(),
            "```json".to_string(),
            r#"[{"name": "<tool_name>", "arguments": {<arguments>}}]"#.to_string(),
            "```".to_string(),
        ];
        if tc_mode == "forced" {
            let n = tc_name.as_deref().unwrap_or("tool");
            tool_instructions.push(format!("- MANDATORY: You MUST call the tool '{}'.", n));
            tool_instructions.push(
                "- Do not reply with plain text; always emit the JSON code block.".to_string(),
            );
        } else if tc_mode == "required" {
            tool_instructions.push("- MANDATORY: You MUST call at least one tool.".to_string());
            tool_instructions.push(
                "- Do not reply with plain text; always emit the JSON code block.".to_string(),
            );
        } else {
            tool_instructions.push("- If no tool is needed or appropriate, reply with conversational text without JSON.".to_string());
        }
        system_parts.push(tool_instructions.join("\n"));
    }

    let prompt = turns.join("\n\n");

    // Diagnostic: log per-component sizes to trace prompt bloat
    {
        let client_sys_len: usize = if norm_tools.is_empty() {
            system_parts.iter().map(|s| s.len()).sum()
        } else if system_parts.len() > 1 {
            system_parts.iter().rev().skip(1).map(|s| s.len()).sum()
        } else {
            0
        };
        let tool_schema_len: usize = if !norm_tools.is_empty() {
            system_parts.last().map(|s| s.len()).unwrap_or(0)
        } else {
            0
        };
        let largest_turn = turns.iter().map(|t| t.len()).max().unwrap_or(0);
        info!(
            "anthropic_to_uds: turns={}, client_system_len={}, tool_schema_len={}, prompt_len={}, largest_turn={}",
            turns.len(), client_sys_len, tool_schema_len, prompt.len(), largest_turn
        );
    }

    let system = if !system_parts.is_empty() {
        Some(system_parts.join("\n\n"))
    } else {
        None
    };

    let images_opt = if images.is_empty() {
        None
    } else {
        Some(images)
    };

    UdsInferenceRequest {
        kind: "complete".to_string(),
        request_id: format!("gw-{}", Uuid::new_v4()),
        slug: default_slug.to_string(),
        prompt,
        timeout_secs: 180,
        system,
        images: images_opt,
    }
}

pub fn extract_uds_content_text(uds_resp: &UdsResponse) -> (String, u64, u64, u64) {
    if let Some(res) = &uds_resp.result {
        let text = match &res.content {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Object(map)) => {
                if let Some(Value::String(s)) = map.get("text") {
                    s.clone()
                } else {
                    res.content
                        .as_ref()
                        .map(|v| v.to_string())
                        .unwrap_or_default()
                }
            }
            Some(v) => v.to_string(),
            None => "".to_string(),
        };
        let usage = res.usage.as_ref();
        let inp = usage.map(|u| u.input_tokens).unwrap_or(0);
        let out = usage.map(|u| u.output_tokens).unwrap_or(0);
        let tot = usage.map(|u| u.total_tokens).unwrap_or(inp + out);
        (text, inp, out, tot)
    } else {
        ("".to_string(), 0, 0, 0)
    }
}

pub fn format_uds_to_openai(
    uds_resp: &UdsResponse,
    requested_model: Option<&str>,
    is_tool: bool,
    tool_calls: &[ToolCall],
    clean_text: Option<&str>,
) -> Value {
    let model = requested_model.unwrap_or("muse");
    let cid = format!("chatcmpl-{}", Uuid::new_v4().simple());
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let (_, in_tokens, out_tokens, total_tokens) = extract_uds_content_text(uds_resp);

    let mut msg_payload = json!({
        "role": "assistant"
    });

    if is_tool {
        msg_payload["content"] = match clean_text {
            Some(txt) if !txt.is_empty() => json!(txt),
            _ => Value::Null,
        };
        msg_payload["tool_calls"] = serde_json::to_value(tool_calls).unwrap_or(json!([]));
    } else {
        msg_payload["content"] = json!(clean_text.unwrap_or(""));
    }

    let finish_reason = if is_tool { "tool_calls" } else { "stop" };

    json!({
        "id": cid,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [
            {
                "index": 0,
                "message": msg_payload,
                "finish_reason": finish_reason
            }
        ],
        "usage": {
            "prompt_tokens": in_tokens,
            "completion_tokens": out_tokens,
            "total_tokens": total_tokens
        }
    })
}

pub fn format_uds_to_openai_stream(
    uds_resp: &UdsResponse,
    requested_model: Option<&str>,
    is_tool: bool,
    tool_calls: &[ToolCall],
    clean_text: Option<&str>,
) -> String {
    let model = requested_model.unwrap_or("muse");
    let cid = format!("chatcmpl-{}", Uuid::new_v4().simple());
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let (_, in_tokens, out_tokens, total_tokens) = extract_uds_content_text(uds_resp);

    let mut chunks = Vec::new();

    if is_tool {
        // Chunk 1: Initial tool call descriptors with empty arguments
        let initial_tcs: Vec<Value> = tool_calls
            .iter()
            .enumerate()
            .map(|(i, tc)| {
                json!({
                    "index": i,
                    "id": tc.id,
                    "type": "function",
                    "function": {
                        "name": tc.function.name,
                        "arguments": ""
                    }
                })
            })
            .collect();

        let delta1 = if let Some(txt) = clean_text {
            json!({
                "role": "assistant",
                "content": txt,
                "tool_calls": initial_tcs
            })
        } else {
            json!({
                "role": "assistant",
                "tool_calls": initial_tcs
            })
        };

        chunks.push(json!({
            "id": cid,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": delta1,
                "finish_reason": Value::Null
            }]
        }));

        // Chunk 2: Tool call arguments
        let arg_tcs: Vec<Value> = tool_calls
            .iter()
            .enumerate()
            .map(|(i, tc)| {
                json!({
                    "index": i,
                    "function": {
                        "arguments": tc.function.arguments
                    }
                })
            })
            .collect();

        chunks.push(json!({
            "id": cid,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": arg_tcs
                },
                "finish_reason": Value::Null
            }]
        }));

        // Chunk 3: Final chunk with finish_reason tool_calls and usage
        chunks.push(json!({
            "id": cid,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": in_tokens,
                "completion_tokens": out_tokens,
                "total_tokens": total_tokens
            }
        }));
    } else {
        // Normal text streaming
        // Chunk 1: Role
        chunks.push(json!({
            "id": cid,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {
                    "role": "assistant"
                },
                "finish_reason": Value::Null
            }]
        }));

        // Chunk 2: Content
        chunks.push(json!({
            "id": cid,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {
                    "content": clean_text.unwrap_or("")
                },
                "finish_reason": Value::Null
            }]
        }));

        // Chunk 3: Stop with usage
        chunks.push(json!({
            "id": cid,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": in_tokens,
                "completion_tokens": out_tokens,
                "total_tokens": total_tokens
            }
        }));
    }

    let mut out = String::new();
    for chunk in chunks {
        out.push_str(&format!(
            "data: {}\n\n",
            serde_json::to_string(&chunk).unwrap_or_default()
        ));
    }
    out.push_str("data: [DONE]\n\n");
    out
}

pub fn format_uds_to_anthropic(
    uds_resp: &UdsResponse,
    requested_model: Option<&str>,
    is_tool: bool,
    tool_calls: &[ToolCall],
    clean_text: Option<&str>,
) -> Value {
    let model = requested_model.unwrap_or("claude-3-5-sonnet-20241022");
    let msg_id = format!("msg_{}", Uuid::new_v4().simple());

    let (_, in_tokens, out_tokens, _) = extract_uds_content_text(uds_resp);

    let mut blocks = Vec::new();
    if let Some(txt) = clean_text {
        if !txt.is_empty() {
            blocks.push(json!({
                "type": "text",
                "text": txt
            }));
        }
    }

    let stop_reason = if is_tool {
        for tc in tool_calls {
            let valid_id = if tc.id.is_empty() {
                format!("toolu_{}", Uuid::new_v4().simple())
            } else if tc.id.starts_with("call_") {
                format!("toolu_{}", &tc.id[5..])
            } else {
                tc.id.clone()
            };

            let input_val: Value = serde_json::from_str::<Value>(&tc.function.arguments)
                .ok()
                .filter(|v| v.is_object())
                .unwrap_or_else(|| json!({}));

            blocks.push(json!({
                "type": "tool_use",
                "id": valid_id,
                "name": tc.function.name,
                "input": input_val
            }));
        }
        "tool_use"
    } else {
        if blocks.is_empty() {
            blocks.push(json!({
                "type": "text",
                "text": clean_text.unwrap_or("")
            }));
        }
        "end_turn"
    };

    json!({
        "id": msg_id,
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": blocks,
        "stop_reason": stop_reason,
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": in_tokens,
            "output_tokens": out_tokens
        }
    })
}

pub fn format_uds_to_anthropic_stream(
    uds_resp: &UdsResponse,
    requested_model: Option<&str>,
    is_tool: bool,
    tool_calls: &[ToolCall],
    clean_text: Option<&str>,
) -> String {
    let model = requested_model.unwrap_or("claude-3-5-sonnet-20241022");
    let msg_id = format!("msg_{}", Uuid::new_v4().simple());

    let (_, in_tokens, out_tokens, _) = extract_uds_content_text(uds_resp);

    let mut events = Vec::new();

    // 1. message_start
    events.push((
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": msg_id,
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [],
                "stop_reason": Value::Null,
                "stop_sequence": Value::Null,
                "usage": {
                    "input_tokens": in_tokens,
                    "output_tokens": 1
                }
            }
        }),
    ));

    let mut current_block_idx = 0;

    // 2. Text block if present
    if let Some(txt) = clean_text {
        if !txt.is_empty() || !is_tool {
            events.push((
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": current_block_idx,
                    "content_block": {
                        "type": "text",
                        "text": ""
                    }
                }),
            ));
            events.push((
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": current_block_idx,
                    "delta": {
                        "type": "text_delta",
                        "text": txt
                    }
                }),
            ));
            events.push((
                "content_block_stop",
                json!({
                    "type": "content_block_stop",
                    "index": current_block_idx
                }),
            ));
            current_block_idx += 1;
        }
    }

    // 3. Tool use blocks if present
    let stop_reason = if is_tool {
        for tc in tool_calls {
            let valid_id = if tc.id.is_empty() {
                format!("toolu_{}", Uuid::new_v4().simple())
            } else if tc.id.starts_with("call_") {
                format!("toolu_{}", &tc.id[5..])
            } else {
                tc.id.clone()
            };

            let valid_json_args = {
                let trimmed = tc.function.arguments.trim();
                if trimmed.starts_with('{') && trimmed.ends_with('}') {
                    if serde_json::from_str::<Value>(trimmed)
                        .map(|v| v.is_object())
                        .unwrap_or(false)
                    {
                        trimmed.to_string()
                    } else {
                        "{}".to_string()
                    }
                } else {
                    "{}".to_string()
                }
            };

            events.push((
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": current_block_idx,
                    "content_block": {
                        "type": "tool_use",
                        "id": valid_id,
                        "name": tc.function.name,
                        "input": {}
                    }
                }),
            ));
            events.push((
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": current_block_idx,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": valid_json_args
                    }
                }),
            ));
            events.push((
                "content_block_stop",
                json!({
                    "type": "content_block_stop",
                    "index": current_block_idx
                }),
            ));
            current_block_idx += 1;
        }
        "tool_use"
    } else {
        "end_turn"
    };

    // 4. message_delta
    events.push((
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": stop_reason,
                "stop_sequence": Value::Null
            },
            "usage": {
                "output_tokens": out_tokens
            }
        }),
    ));

    // 5. message_stop
    events.push((
        "message_stop",
        json!({
            "type": "message_stop"
        }),
    ));

    let mut out = String::new();
    for (event_name, data) in events {
        out.push_str(&format!(
            "event: {}\ndata: {}\n\n",
            event_name,
            serde_json::to_string(&data).unwrap_or_default()
        ));
    }
    out
}
