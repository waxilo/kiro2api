//! OpenAI Responses API 兼容层：`POST /openai/v1/responses`。
//!
//! 目标客户是 Codex CLI：2026-02 起 Codex 移除了 `wire_api = "chat"`，只讲
//! Responses API（openai/codex#7782），本代理必须提供该形状的入口才能接入 Codex。
//!
//! 与 chat/completions 的关系仍是「同一管线的第三个入口」：
//!
//! ```text
//! Responses 请求 ──(responses_request_to_anthropic)──► Anthropic body
//!                          │（先折算成 Chat 形状，再走 openai_request_to_anthropic）
//!                          ▼
//!          build_kiro_request / 去重 / 限流 / 健康自愈 / 工具恢复
//!                          ▼
//! Responses 事件 ◄──(ResponsesEmitter)──────────── CollectedText / KiroEvent
//! ```
//!
//! 入参方向复用 chat 那条归一化链：Responses 与 Chat 的形状差异（`instructions`
//! 字段、`input` items、平铺的 tools / tool_choice）先折算成 Chat 形状，之后全部
//! 下游逻辑（system 合并、tool schema 校验、antml 泄漏恢复）原样生效。
//!
//! 出参方向是真正的协议翻译：Responses 的流是「事件 = 带 type 的 JSON 对象」，
//! 正文按 output item 组织（message / function_call）。思维链（reasoningContentEvent）
//! 本期不产出 —— Responses 侧需要 reasoning item / summary 事件信封，先不做；
//! Codex 拿到纯文本与工具调用即可正常工作。
//!
//! 与 Chat 流的另一点差异：Responses 官方**不发** `data: [DONE]`，以
//! `response.completed` 事件收尾，连接自然关闭。

use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::kiro::openai::openai_request_to_anthropic;

// ============ 入参：Responses → Anthropic ============

/// 把 Responses 请求体翻译成 Anthropic Messages 请求体。
///
/// 中途经过一层 Chat 形状（`responses_request_to_chat`），以复用
/// `openai_request_to_anthropic` 的全部下游处理。
pub fn responses_request_to_anthropic(body: &Value) -> Result<Value, String> {
    let chat = responses_request_to_chat(body)?;
    openai_request_to_anthropic(&chat)
}

/// Responses 请求 → Chat Completions 形状的请求体。
///
/// 覆盖的字段：
/// - `instructions` → 一条 system 消息
/// - `input`（字符串或 item 数组）→ `messages[]`；`reasoning` 等 status 类 item 丢弃
/// - `tools[]`（平铺的 `{type:"function", name, parameters}`）→ Chat 的嵌套形状；
///   `web_search` / `local_shell` / `custom` 等 hosted 工具本代理无法代答，直接丢弃
/// - `tool_choice`（平铺 `{type:"function", name}`）→ Chat 的嵌套形状
/// - `max_output_tokens` → `max_tokens`
/// - `reasoning.effort` → `reasoning_effort`
///
/// `stream` / `store` / `include` / `metadata` 由调用方或下游自行处理，不在此消费。
fn responses_request_to_chat(body: &Value) -> Result<Value, String> {
    let mut messages: Vec<Value> = Vec::new();
    if let Some(instructions) = body.get("instructions").and_then(Value::as_str) {
        if !instructions.trim().is_empty() {
            messages.push(json!({ "role": "system", "content": instructions }));
        }
    }
    match body.get("input") {
        Some(Value::String(text)) => {
            if !text.is_empty() {
                messages.push(json!({ "role": "user", "content": text }));
            }
        }
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(message) = response_item_to_chat_message(item) {
                    messages.push(message);
                }
            }
        }
        _ => {}
    }

    let mut chat = Map::new();
    if let Some(model) = body.get("model").and_then(Value::as_str) {
        chat.insert("model".to_string(), json!(model));
    }
    chat.insert("messages".to_string(), Value::Array(messages));

    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        let converted = tools.iter().filter_map(responses_tool_to_chat).collect::<Vec<_>>();
        if !converted.is_empty() {
            chat.insert("tools".to_string(), Value::Array(converted));
        }
    }
    if let Some(choice) = body.get("tool_choice") {
        if let Some(normalized) = responses_tool_choice_to_chat(choice) {
            chat.insert("tool_choice".to_string(), normalized);
        }
    }
    if let Some(max_tokens) = body.get("max_output_tokens").and_then(Value::as_i64) {
        chat.insert("max_tokens".to_string(), json!(max_tokens));
    }
    if let Some(effort) = body
        .get("reasoning")
        .and_then(|reasoning| reasoning.get("effort"))
        .and_then(Value::as_str)
    {
        if !effort.trim().is_empty() {
            chat.insert("reasoning_effort".to_string(), json!(effort));
        }
    }
    Ok(Value::Object(chat))
}

/// 单个 input item → Chat 消息。无法承载的 item（reasoning、item_reference 等）返回 None。
fn response_item_to_chat_message(item: &Value) -> Option<Value> {
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
    match item_type {
        "message" => {
            let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
            let content = item.get("content").unwrap_or(&Value::Null);
            match role {
                "system" | "developer" => Some(json!({
                    "role": "system",
                    "content": responses_content_to_text(content),
                })),
                "assistant" => Some(json!({
                    "role": "assistant",
                    "content": responses_content_to_text(content),
                })),
                _ => Some(json!({
                    "role": "user",
                    "content": responses_content_to_chat_parts(content),
                })),
            }
        }
        "function_call" => {
            let name = item.get("name").and_then(Value::as_str).unwrap_or("").trim();
            if name.is_empty() {
                return None;
            }
            Some(json!({
                "role": "assistant",
                "content": Value::Null,
                "tool_calls": [{
                    "id": item.get("call_id").and_then(Value::as_str).unwrap_or(""),
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": item.get("arguments").and_then(Value::as_str).unwrap_or(""),
                    },
                }],
            }))
        }
        "function_call_output" => Some(json!({
            "role": "tool",
            "tool_call_id": item.get("call_id").and_then(Value::as_str).unwrap_or(""),
            "content": responses_output_to_text(item.get("output")),
        })),
        // reasoning（历史思维链）、item_reference、local_shell_call 等一律忽略
        _ => None,
    }
}

/// Responses `content`（字符串 / part 数组）→ 纯文本。
pub fn responses_content_to_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    let Some(parts) = content.as_array() else {
        return String::new();
    };
    parts
        .iter()
        .filter_map(|part| {
            let part_type = part.get("type").and_then(Value::as_str).unwrap_or("");
            match part_type {
                "input_text" | "output_text" | "summary_text" | "text" => {
                    part.get("text").and_then(Value::as_str).map(str::to_string)
                }
                _ => None,
            }
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Responses 用户消息 `content` → Chat 的 content part 数组（支持 `input_image`）。
fn responses_content_to_chat_parts(content: &Value) -> Value {
    if let Some(text) = content.as_str() {
        return json!([{ "type": "text", "text": text }]);
    }
    let Some(parts) = content.as_array() else {
        return json!([]);
    };
    let mut out = Vec::new();
    for part in parts {
        match part.get("type").and_then(Value::as_str).unwrap_or("") {
            "input_text" | "output_text" | "text" => {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        out.push(json!({ "type": "text", "text": text }));
                    }
                }
            }
            "input_image" => {
                // image_url 可能是字符串（常见，data: URL）或 {url} 对象
                let url = match part.get("image_url") {
                    Some(Value::String(url)) => Some(url.clone()),
                    Some(value @ Value::Object(_)) => value.get("url").and_then(Value::as_str).map(str::to_string),
                    _ => None,
                };
                if let Some(url) = url {
                    out.push(json!({ "type": "image_url", "image_url": { "url": url } }));
                }
            }
            _ => {}
        }
    }
    Value::Array(out)
}

/// `function_call_output.output` → 纯文本。规范是字符串；对象/数组做兜底序列化。
fn responses_output_to_text(output: Option<&Value>) -> String {
    match output {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(value) => serde_json::to_string(value).unwrap_or_default(),
        None => String::new(),
    }
}

/// Responses 的平铺 function 工具 → Chat 的嵌套形状；hosted 工具丢弃。
fn responses_tool_to_chat(tool: &Value) -> Option<Value> {
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        return None;
    }
    let name = tool.get("name").and_then(Value::as_str)?.trim();
    if name.is_empty() {
        return None;
    }
    let mut function = Map::new();
    function.insert("name".to_string(), json!(name));
    if let Some(description) = tool.get("description").and_then(Value::as_str) {
        if !description.is_empty() {
            function.insert("description".to_string(), json!(description));
        }
    }
    function.insert(
        "parameters".to_string(),
        tool.get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
    );
    if let Some(strict) = tool.get("strict") {
        function.insert("strict".to_string(), strict.clone());
    }
    Some(json!({ "type": "function", "function": Value::Object(function) }))
}

/// Responses 的平铺 tool_choice → Chat 的嵌套形状。
fn responses_tool_choice_to_chat(choice: &Value) -> Option<Value> {
    if choice.is_string() {
        return Some(choice.clone());
    }
    if choice.get("type").and_then(Value::as_str) == Some("function") {
        // Responses 平铺在顶层；容错地也认 Chat 的嵌套形状
        let name = choice
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| choice.get("function").and_then(|f| f.get("name")).and_then(Value::as_str))
            .unwrap_or("")
            .trim();
        if name.is_empty() {
            return None;
        }
        return Some(json!({ "type": "function", "function": { "name": name } }));
    }
    None
}

// ============ 出参：SSE 事件 ============

/// Responses 的 SSE 帧：官方带 `event:` 行，data 内 `type` 与之相同。
pub fn responses_sse(event_type: &str, payload: &Value) -> String {
    format!("event: {event_type}\ndata: {payload}\n\n")
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Responses 形状的 usage。上游只有输出侧可估（与 chat 路径同一把尺），
/// 输入侧没有可靠来源，置 0 —— Codex 只把它用于统计展示。
pub fn responses_usage(output_tokens: i64) -> Value {
    json!({
        "input_tokens": 0,
        "input_tokens_details": { "cached_tokens": 0 },
        "output_tokens": output_tokens,
        "output_tokens_details": { "reasoning_tokens": 0 },
        "total_tokens": output_tokens,
    })
}

/// 把 Kiro 的产出翻译成 Responses 事件流的状态机。
///
/// 一次生成里 text 与工具调用可能交错；Responses 的 output 是有序数组，
/// 因此规则是：同一时刻至多一个 open 中的 item，新 item 开始前先收口当前 item
/// （文本 → function_call 是主流走向，反之亦然）。收口的 item 进入 `output`，
/// 其下标即它在最终数组里的位置 —— 与官方实现的 output_index 语义一致。
pub struct ResponsesEmitter {
    response_id: String,
    model: String,
    sequence: u64,
    output: Vec<Value>,
    message: Option<OpenMessage>,
    function_call: Option<OpenFunctionCall>,
}

struct OpenMessage {
    id: String,
    output_index: usize,
    text: String,
}

struct OpenFunctionCall {
    id: String,
    output_index: usize,
    call_id: String,
    name: String,
    arguments: String,
}

impl ResponsesEmitter {
    pub fn new(response_id: &str, model: &str) -> Self {
        Self {
            response_id: response_id.to_string(),
            model: model.to_string(),
            sequence: 0,
            output: Vec::new(),
            message: None,
            function_call: None,
        }
    }

    fn frame(&mut self, event_type: &str, mut payload: Value) -> String {
        let sequence = self.sequence;
        self.sequence += 1;
        if let Some(object) = payload.as_object_mut() {
            object.insert("type".to_string(), json!(event_type));
            object.insert("sequence_number".to_string(), json!(sequence));
        }
        responses_sse(event_type, &payload)
    }

    /// 当前时刻的 response 对象快照（status 由调用方给定）。
    fn snapshot(&self, status: &str) -> Value {
        json!({
            "id": self.response_id,
            "object": "response",
            "created_at": now_unix(),
            "status": status,
            "error": Value::Null,
            "incomplete_details": Value::Null,
            "instructions": Value::Null,
            "model": self.model,
            "output": self.output,
            "parallel_tool_calls": true,
            "previous_response_id": Value::Null,
            "reasoning": { "effort": Value::Null, "summary": Value::Null },
            "store": false,
            "temperature": Value::Null,
            "text": { "format": { "type": "text" } },
            "tool_choice": "auto",
            "tools": [],
            "top_p": Value::Null,
            "usage": Value::Null,
            "metadata": {},
        })
    }

    pub fn event_created(&mut self) -> String {
        self.frame("response.created", json!({ "response": self.snapshot("in_progress") }))
    }

    pub fn event_in_progress(&mut self) -> String {
        self.frame("response.in_progress", json!({ "response": self.snapshot("in_progress") }))
    }

    /// 开启 message item（幂等：已开着则什么都不发）。
    /// 若有 open 中的 function_call 先收口它 —— 同一时刻至多一个 open item，
    /// 否则两个 item 会争用同一个 output_index。
    pub fn open_message(&mut self, send: &mut impl FnMut(String)) {
        self.close_function_call(send);
        if self.message.is_some() {
            return;
        }
        let id = format!("msg_{}", Uuid::new_v4().simple());
        let output_index = self.output.len();
        self.message = Some(OpenMessage {
            id: id.clone(),
            output_index,
            text: String::new(),
        });
        send(self.frame(
            "response.output_item.added",
            json!({
                "output_index": output_index,
                "item": { "id": id, "type": "message", "status": "in_progress", "role": "assistant", "content": [] },
            }),
        ));
        send(self.frame(
            "response.content_part.added",
            json!({
                "item_id": id,
                "output_index": output_index,
                "content_index": 0,
                "part": { "type": "output_text", "text": "", "annotations": [] },
            }),
        ));
    }

    pub fn push_text(&mut self, send: &mut impl FnMut(String), delta: &str) {
        let (item_id, output_index) = {
            let Some(message) = self.message.as_mut() else {
                return;
            };
            message.text.push_str(delta);
            (message.id.clone(), message.output_index)
        };
        send(self.frame(
            "response.output_text.delta",
            json!({
                "item_id": item_id,
                "output_index": output_index,
                "content_index": 0,
                "delta": delta,
                "logprobs": [],
            }),
        ));
    }

    /// 收口当前 message item；没有 open 的 message 时为空操作。
    pub fn close_message(&mut self, send: &mut impl FnMut(String)) {
        let Some(message) = self.message.take() else {
            return;
        };
        let text = message.text;
        send(self.frame(
            "response.output_text.done",
            json!({
                "item_id": message.id,
                "output_index": message.output_index,
                "content_index": 0,
                "text": text,
            }),
        ));
        send(self.frame(
            "response.content_part.done",
            json!({
                "item_id": message.id,
                "output_index": message.output_index,
                "content_index": 0,
                "part": { "type": "output_text", "text": text, "annotations": [] },
            }),
        ));
        let item = json!({
            "id": message.id,
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": text, "annotations": [] }],
        });
        send(self.frame(
            "response.output_item.done",
            json!({ "output_index": message.output_index, "item": item }),
        ));
        self.output.push(item);
    }

    /// 开启 function_call item；文本先行时先收口 message。
    pub fn open_function_call(&mut self, send: &mut impl FnMut(String), call_id: &str, name: &str) {
        self.close_message(send);
        let id = format!("fc_{}", Uuid::new_v4().simple());
        let output_index = self.output.len();
        self.function_call = Some(OpenFunctionCall {
            id: id.clone(),
            output_index,
            call_id: call_id.to_string(),
            name: name.to_string(),
            arguments: String::new(),
        });
        send(self.frame(
            "response.output_item.added",
            json!({
                "output_index": output_index,
                "item": {
                    "id": id,
                    "type": "function_call",
                    "status": "in_progress",
                    "call_id": call_id,
                    "name": name,
                    "arguments": "",
                },
            }),
        ));
    }

    pub fn push_function_arguments(&mut self, send: &mut impl FnMut(String), fragment: &str) {
        let (item_id, output_index) = {
            let Some(call) = self.function_call.as_mut() else {
                return;
            };
            call.arguments.push_str(fragment);
            (call.id.clone(), call.output_index)
        };
        send(self.frame(
            "response.function_call_arguments.delta",
            json!({
                "item_id": item_id,
                "output_index": output_index,
                "delta": fragment,
            }),
        ));
    }

    /// 收口当前 function_call item；没有 open 的调用时为空操作。
    pub fn close_function_call(&mut self, send: &mut impl FnMut(String)) {
        let Some(call) = self.function_call.take() else {
            return;
        };
        let arguments = call.arguments;
        send(self.frame(
            "response.function_call_arguments.done",
            json!({
                "item_id": call.id,
                "output_index": call.output_index,
                "arguments": arguments,
            }),
        ));
        let item = json!({
            "id": call.id,
            "type": "function_call",
            "status": "completed",
            "call_id": call.call_id,
            "name": call.name,
            "arguments": arguments,
        });
        send(self.frame(
            "response.output_item.done",
            json!({ "output_index": call.output_index, "item": item }),
        ));
        self.output.push(item);
    }

    /// `incomplete_details`：流被截断（如 max_tokens）时给 `Some("max_tokens")`，
    /// 正常结束给 None —— 快照里相应置 null / `{"reason": …}`。
    pub fn event_completed(&mut self, usage: Value, incomplete_details: Option<&str>) -> String {
        let mut snapshot = self.snapshot("completed");
        if let Some(object) = snapshot.as_object_mut() {
            object.insert("usage".to_string(), usage);
            object.insert(
                "incomplete_details".to_string(),
                match incomplete_details {
                    Some(reason) => json!({ "reason": reason }),
                    None => Value::Null,
                },
            );
        }
        self.frame("response.completed", json!({ "response": snapshot }))
    }

    pub fn event_failed(&mut self, code: &str, message: &str) -> String {
        let mut snapshot = self.snapshot("failed");
        if let Some(object) = snapshot.as_object_mut() {
            object.insert(
                "error".to_string(),
                json!({ "code": code, "message": message }),
            );
        }
        self.frame("response.failed", json!({ "response": snapshot }))
    }
}

// ============ 出参：非流式响应 ============

/// Anthropic `content[]` 块 → Responses 的 output items。
/// 文本块合并进一个 message item；`tool_use` 块逐个变成 function_call item。
pub fn build_responses_output(content: &[Value]) -> Vec<Value> {
    let mut output: Vec<Value> = Vec::new();
    let mut text = String::new();
    let flush_text = |output: &mut Vec<Value>, text: &mut String| {
        if text.is_empty() {
            return;
        }
        output.push(json!({
            "id": format!("msg_{}", Uuid::new_v4().simple()),
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": text.clone(), "annotations": [] }],
        }));
        text.clear();
    };
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            // thinking 块不进 output：Responses 侧需要独立的 reasoning item 信封
            Some("text") => {
                if let Some(part) = block.get("text").and_then(Value::as_str) {
                    text.push_str(part);
                }
            }
            Some("tool_use") => {
                flush_text(&mut output, &mut text);
                let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                if name.is_empty() {
                    continue;
                }
                let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                output.push(json!({
                    "id": format!("fc_{}", Uuid::new_v4().simple()),
                    "type": "function_call",
                    "status": "completed",
                    "call_id": block.get("id").and_then(Value::as_str).unwrap_or(""),
                    "name": name,
                    "arguments": serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string()),
                }));
            }
            _ => {}
        }
    }
    flush_text(&mut output, &mut text);
    output
}

/// 组装非流式的完整 response 对象（status completed + usage）。
pub fn responses_completed_response(
    response_id: &str,
    model: &str,
    output: Vec<Value>,
    output_tokens: i64,
) -> Value {
    let mut emitter = ResponsesEmitter::new(response_id, model);
    emitter.output = output;
    let mut snapshot = emitter.snapshot("completed");
    if let Some(object) = snapshot.as_object_mut() {
        object.insert("usage".to_string(), responses_usage(output_tokens));
    }
    snapshot
}

/// 流式路径里、尚未发过任何事件时的失败收尾：created + failed 两帧。
/// 已发过部分内容的场景由管道内部用自己的 emitter 收口（快照里保留已产出的 items）。
pub fn finish_responses_after_error(
    mut send: impl FnMut(String),
    response_id: &str,
    model: &str,
    message: &str,
) {
    let mut emitter = ResponsesEmitter::new(response_id, model);
    send(emitter.event_created());
    send(emitter.event_failed("upstream_error", message));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn converts_instructions_and_string_input() {
        let body = json!({
            "model": "claude-sonnet-4-5",
            "instructions": "be terse",
            "input": "hello",
            "max_output_tokens": 1024,
            "reasoning": { "effort": "HIGH" },
        });
        let out = responses_request_to_anthropic(&body).unwrap();
        assert_eq!(out["system"], "be terse");
        assert_eq!(out["messages"][0]["role"], "user");
        assert_eq!(out["messages"][0]["content"][0]["text"], "hello");
        assert_eq!(out["max_tokens"], 1024);
        assert_eq!(out["output_config"]["effort"], "high");
    }

    #[test]
    fn converts_item_array_round_trip() {
        let body = json!({
            "model": "m",
            "input": [
                { "type": "message", "role": "user", "content": "list files" },
                { "type": "reasoning", "summary": [] },
                { "type": "function_call", "call_id": "call_1", "name": "Bash", "arguments": "{\"command\":\"ls\"}" },
                { "type": "function_call_output", "call_id": "call_1", "output": "-rw a.txt" },
            ],
            "tools": [
                { "type": "function", "name": "Bash", "description": "run a command",
                  "parameters": { "type": "object", "properties": { "command": { "type": "string" } } } },
                { "type": "web_search" },
            ],
        });
        let out = responses_request_to_anthropic(&body).unwrap();
        // reasoning item 被丢弃；function_call → assistant 的 tool_use；output → tool_result
        assert_eq!(out["messages"].as_array().unwrap().len(), 3);
        assert_eq!(out["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(out["messages"][1]["content"][0]["input"]["command"], "ls");
        assert_eq!(out["messages"][2]["content"][0]["type"], "tool_result");
        assert_eq!(out["messages"][2]["content"][0]["tool_use_id"], "call_1");
        // 平铺的 function 工具折成嵌套形状并进 schema；web_search 被丢弃
        assert_eq!(out["tools"].as_array().unwrap().len(), 1);
        assert_eq!(out["tools"][0]["name"], "Bash");
        assert_eq!(out["tools"][0]["input_schema"]["properties"]["command"]["type"], "string");
    }

    #[test]
    fn converts_user_content_parts_and_images() {
        let body = json!({
            "model": "m",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [
                    { "type": "input_text", "text": "what is this" },
                    { "type": "input_image", "image_url": "data:image/png;base64,AAAA" },
                    { "type": "input_image", "image_url": { "url": "data:image/jpeg;base64,BBBB" } },
                ],
            }],
        });
        let out = responses_request_to_anthropic(&body).unwrap();
        let content = out["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 3);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["source"]["data"], "AAAA");
        assert_eq!(content[2]["source"]["media_type"], "image/jpeg");
    }

    #[test]
    fn converts_flat_tool_choice() {
        let body = json!({
            "model": "m",
            "input": "hi",
            "tool_choice": { "type": "function", "name": "Bash" },
        });
        let out = responses_request_to_anthropic(&body).unwrap();
        assert_eq!(out["tool_choice"]["type"], "tool");
        assert_eq!(out["tool_choice"]["name"], "Bash");
    }

    #[test]
    fn rejects_input_without_user_turn() {
        let body = json!({ "model": "m", "instructions": "x" });
        assert!(responses_request_to_anthropic(&body).is_err());
    }

    #[test]
    fn sse_frames_carry_event_line_and_type() {
        let frame = responses_sse("response.created", &json!({ "type": "response.created" }));
        assert!(frame.starts_with("event: response.created\ndata: "));
        assert!(frame.ends_with("\n\n"));
    }

    #[test]
    fn emitter_streams_text_then_tool_call() {
        let mut emitter = ResponsesEmitter::new("resp_1", "m");
        let mut frames: Vec<String> = Vec::new();
        {
            let mut send = |frame: String| frames.push(frame);
            emitter.open_message(&mut send);
            emitter.push_text(&mut send, "Hel");
            emitter.push_text(&mut send, "lo");
            emitter.open_function_call(&mut send, "call_9", "Bash");
            emitter.push_function_arguments(&mut send, "{\"a\":");
            emitter.push_function_arguments(&mut send, "1}");
            emitter.close_function_call(&mut send);
            frames.push(emitter.event_completed(responses_usage(5), None));
        }

        let types: Vec<String> = frames
            .iter()
            .filter_map(|frame| {
                let data = frame.strip_prefix("event: ")?.split("\ndata: ").nth(1)?;
                let parsed: Value = serde_json::from_str(data.trim_end()).ok()?;
                parsed
                    .get("type")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        assert_eq!(
            types,
            vec![
                "response.output_item.added",
                "response.content_part.added",
                "response.output_text.delta",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.output_item.added",
                "response.function_call_arguments.delta",
                "response.function_call_arguments.delta",
                "response.function_call_arguments.done",
                "response.output_item.done",
                "response.completed",
            ]
        );

        let completed: Value = serde_json::from_str(
            frames
                .last()
                .unwrap()
                .split("\ndata: ")
                .nth(1)
                .unwrap()
                .trim_end(),
        )
        .unwrap();
        let response = &completed["response"];
        assert_eq!(response["status"], "completed");
        assert_eq!(response["output"].as_array().unwrap().len(), 2);
        assert_eq!(response["output"][0]["type"], "message");
        assert_eq!(response["output"][0]["content"][0]["text"], "Hello");
        assert_eq!(response["output"][1]["type"], "function_call");
        assert_eq!(response["output"][1]["call_id"], "call_9");
        assert_eq!(response["output"][1]["arguments"], "{\"a\":1}");
        assert_eq!(response["usage"]["output_tokens"], 5);

        // sequence_number 严格递增
        let sequences: Vec<u64> = frames
            .iter()
            .filter_map(|frame| {
                let data = frame.split("\ndata: ").nth(1)?;
                serde_json::from_str::<Value>(data.trim_end())
                    .ok()?
                    .get("sequence_number")
                    .and_then(Value::as_u64)
            })
            .collect();
        let sorted = {
            let mut sorted = sequences.clone();
            sorted.sort_unstable();
            sorted
        };
        assert_eq!(sequences, sorted);
    }

    #[test]
    fn emitter_close_message_is_idempotent_and_text_resumes_as_new_item() {
        let mut emitter = ResponsesEmitter::new("resp_1", "m");
        let mut count = 0usize;
        {
            let mut send = |_frame: String| count += 1;
            emitter.push_text(&mut send, "x"); // 没有 open 的 message：静默丢弃
            emitter.open_message(&mut send);
            emitter.push_text(&mut send, "a");
            emitter.close_message(&mut send);
            emitter.close_message(&mut send); // 幂等
            emitter.open_message(&mut send);
            emitter.push_text(&mut send, "b");
            emitter.close_message(&mut send);
        }
        let completed = emitter.event_completed(responses_usage(2), None);
        let response = completed.split("\ndata: ").nth(1).unwrap();
        let parsed: Value = serde_json::from_str(response.trim_end()).unwrap();
        let output = parsed["response"]["output"].as_array().unwrap();
        assert_eq!(output.len(), 2);
        assert_eq!(output[0]["content"][0]["text"], "a");
        assert_eq!(output[1]["content"][0]["text"], "b");
        assert!(count > 0);
    }

    #[test]
    fn failed_event_carries_error_and_keeps_partial_output() {
        let mut emitter = ResponsesEmitter::new("resp_1", "m");
        let mut frames: Vec<String> = Vec::new();
        {
            let mut send = |frame: String| frames.push(frame);
            emitter.open_message(&mut send);
            emitter.push_text(&mut send, "partial");
            emitter.close_message(&mut send);
            frames.push(emitter.event_failed("upstream_error", "boom"));
        }
        let last = frames.last().unwrap().split("\ndata: ").nth(1).unwrap();
        let parsed: Value = serde_json::from_str(last.trim_end()).unwrap();
        let response = &parsed["response"];
        assert_eq!(response["status"], "failed");
        assert_eq!(response["error"]["message"], "boom");
        assert_eq!(response["output"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn builds_non_stream_output_from_anthropic_content() {
        let content = vec![
            json!({ "type": "thinking", "thinking": "hmm" }),
            json!({ "type": "text", "text": "checking" }),
            json!({ "type": "tool_use", "id": "call_1", "name": "Read", "input": { "path": "a.txt" } }),
        ];
        let output = build_responses_output(&content);
        assert_eq!(output.len(), 2);
        assert_eq!(output[0]["type"], "message");
        assert_eq!(output[0]["content"][0]["text"], "checking");
        assert_eq!(output[1]["type"], "function_call");
        assert_eq!(output[1]["call_id"], "call_1");
        let arguments = output[1]["arguments"].as_str().unwrap();
        assert_eq!(serde_json::from_str::<Value>(arguments).unwrap()["path"], "a.txt");
    }

    #[test]
    fn non_stream_response_is_completed_with_usage() {
        let output = build_responses_output(&[json!({ "type": "text", "text": "hi" })]);
        let response = responses_completed_response("resp_1", "m", output, 3);
        assert_eq!(response["object"], "response");
        assert_eq!(response["status"], "completed");
        assert_eq!(response["output"][0]["content"][0]["text"], "hi");
        assert_eq!(response["usage"]["total_tokens"], 3);
    }

    #[test]
    fn finish_after_error_emits_created_then_failed() {
        let mut frames: Vec<String> = Vec::new();
        finish_responses_after_error(|frame| frames.push(frame), "resp_1", "m", "boom");
        assert_eq!(frames.len(), 2);
        assert!(frames[0].starts_with("event: response.created"));
        assert!(frames[1].starts_with("event: response.failed"));
        let failed = frames[1].split("\ndata: ").nth(1).unwrap();
        let parsed: Value = serde_json::from_str(failed.trim_end()).unwrap();
        assert_eq!(parsed["response"]["error"]["message"], "boom");
    }
}
