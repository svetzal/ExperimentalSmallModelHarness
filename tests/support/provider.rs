//! One scripted mock provider for oMLX (OpenAI-compatible) and Ollama.
//!
//! A small HTTP server on an ephemeral port answers each model request with
//! the next scripted reply, streamed in the provider's own wire format, and
//! records every request body for assertions. Replies are deterministic;
//! a `Gate` lets a test hold a stream open until it has seen the screen.
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wire {
    Omlx,
    Ollama,
}

impl Wire {
    pub fn name(self) -> &'static str {
        match self {
            Wire::Omlx => "omlx",
            Wire::Ollama => "ollama",
        }
    }

    /// The request field that carries reasoning effort.
    pub fn effort_field(self) -> &'static str {
        match self {
            Wire::Omlx => "reasoning_effort",
            Wire::Ollama => "think",
        }
    }
}

/// A one-shot latch a test opens to release a held stream.
#[derive(Clone, Default)]
pub struct Gate(Arc<(Mutex<bool>, Condvar)>);

impl Gate {
    pub fn open(&self) {
        let (open, signal) = &*self.0;
        *open.lock().expect("gate") = true;
        signal.notify_all();
    }

    pub fn wait(&self, timeout: Duration) -> bool {
        let (open, signal) = &*self.0;
        let guard = open.lock().expect("gate");
        let (guard, _) = signal
            .wait_timeout_while(guard, timeout, |open| !*open)
            .expect("gate");
        *guard
    }
}

/// One piece of a streamed reply.
#[derive(Clone)]
pub enum Step {
    Thinking(String),
    Content(String),
    /// Tool calls in one frame.
    Tools(Vec<ToolCall>),
    /// Hold the stream until the gate opens.
    Hold(Gate),
    Pause(Duration),
    /// Close the connection without finishing the response.
    Abort,
}

pub fn thinking(text: &str) -> Step {
    Step::Thinking(text.into())
}

pub fn content(text: &str) -> Step {
    Step::Content(text.into())
}

/// One tool call. oMLX calls without an `id` get `call-{request}-{index}`;
/// Ollama has no call ids.
#[derive(Clone)]
pub struct ToolCall {
    pub id: Option<String>,
    pub name: String,
    pub arguments: Value,
}

pub fn tool(name: &str, arguments: Value) -> Step {
    tools(&[(name, arguments)])
}

/// A tool call with a fixed oMLX id, for replies that must be identical
/// whichever request they answer.
pub fn tool_with_id(id: &str, name: &str, arguments: Value) -> Step {
    Step::Tools(vec![ToolCall {
        id: Some(id.into()),
        name: name.into(),
        arguments,
    }])
}

pub fn tools(calls: &[(&str, Value)]) -> Step {
    Step::Tools(
        calls
            .iter()
            .map(|(name, arguments)| ToolCall {
                id: None,
                name: (*name).into(),
                arguments: arguments.clone(),
            })
            .collect(),
    )
}

/// How the server answers one request.
pub enum Reply {
    /// A streamed model response, finished unless it holds an `Abort`.
    Stream(Vec<Step>),
    Json(Value),
    Status(u16),
}

/// What a script sees about a model request.
pub struct ModelRequest<'a> {
    /// 1-based count of model requests so far, including this one.
    pub number: usize,
    pub body: &'a Value,
}

impl ModelRequest<'_> {
    /// The text of the last message, usually the user's prompt.
    pub fn last_message(&self) -> &str {
        self.body["messages"]
            .as_array()
            .and_then(|messages| messages.last())
            .and_then(|message| message["content"].as_str())
            .unwrap_or_default()
    }
}

type Script = dyn Fn(&ModelRequest) -> Reply + Send + Sync;

pub struct MockProvider {
    wire: Wire,
    port: u16,
    requests: Arc<Mutex<Vec<Value>>>,
    gets: Arc<Mutex<Vec<String>>>,
    shows: Arc<Mutex<Vec<Value>>>,
    window: Arc<Mutex<Option<usize>>>,
}

/// Server state shared with each connection.
struct Served {
    wire: Wire,
    recorded: Arc<Mutex<Vec<Value>>>,
    listed: Arc<Mutex<Vec<String>>>,
    shows: Arc<Mutex<Vec<Value>>>,
    window: Arc<Mutex<Option<usize>>>,
    script: Arc<Script>,
    models: Vec<String>,
}

impl MockProvider {
    /// Serve `script` for model requests; GET requests list models.
    pub fn start(
        wire: Wire,
        script: impl Fn(&ModelRequest) -> Reply + Send + Sync + 'static,
    ) -> Self {
        Self::start_with_models(wire, &["test-model", "other-model"], script)
    }

    pub fn start_with_models(
        wire: Wire,
        models: &[&str],
        script: impl Fn(&ModelRequest) -> Reply + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binding the mock provider");
        let port = listener.local_addr().expect("mock address").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let gets = Arc::new(Mutex::new(Vec::new()));
        let shows = Arc::new(Mutex::new(Vec::new()));
        let window = Arc::new(Mutex::new(None));
        let served = Arc::new(Served {
            wire,
            recorded: requests.clone(),
            listed: gets.clone(),
            shows: shows.clone(),
            window: window.clone(),
            script: Arc::new(script),
            models: models.iter().map(|model| (*model).into()).collect(),
        });
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let served = served.clone();
                std::thread::spawn(move || {
                    let _ = serve(stream, &served);
                });
            }
        });
        Self {
            wire,
            port,
            requests,
            gets,
            shows,
            window,
        }
    }

    /// Answer every model request with the same streamed steps.
    pub fn replying(wire: Wire, steps: Vec<Step>) -> Self {
        Self::start(wire, move |_| Reply::Stream(steps.clone()))
    }

    /// Answer the nth model request with the nth turn, then repeat the last.
    pub fn turns(wire: Wire, turns: Vec<Vec<Step>>) -> Self {
        Self::start(wire, move |request| {
            Reply::Stream(turns[(request.number - 1).min(turns.len() - 1)].clone())
        })
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn wire(&self) -> Wire {
        self.wire
    }

    /// Model request bodies received so far.
    pub fn requests(&self) -> Vec<Value> {
        self.requests.lock().expect("requests").clone()
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().expect("requests").len()
    }

    /// Advertise a context window for every model: oMLX's `max_model_len` in
    /// the model list, Ollama's `<arch>.context_length` from `/api/show`.
    pub fn advertise_window(&self, tokens: Option<usize>) {
        *self.window.lock().expect("window") = tokens;
    }

    /// Bodies of Ollama `/api/show` requests, which are not model requests.
    pub fn shows(&self) -> Vec<Value> {
        self.shows.lock().expect("shows").clone()
    }

    /// Paths of GET requests, such as model listings.
    pub fn gets(&self) -> Vec<String> {
        self.gets.lock().expect("gets").clone()
    }

    /// Environment that points both providers at this server.
    pub fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("OMLX_HOST", self.url()),
            ("OLLAMA_HOST", self.url()),
            ("OMLX_API_KEY", "test-key".into()),
        ]
    }
}

fn serve(stream: TcpStream, served: &Served) -> std::io::Result<()> {
    let Served {
        wire,
        recorded,
        listed,
        shows,
        window,
        script,
        models,
    } = served;
    let wire = *wire;
    let window = *window.lock().expect("window");
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    let mut parts = request_line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let mut out = stream;
    if method == "GET" {
        listed.lock().expect("gets").push(path.into());
        let listing = match wire {
            Wire::Omlx => json!({"data": models
                .iter()
                .map(|id| match window {
                    Some(tokens) => json!({"id": id, "max_model_len": tokens}),
                    None => json!({"id": id}),
                })
                .collect::<Vec<_>>()}),
            Wire::Ollama => {
                json!({"models": models.iter().map(|name| json!({"name": name})).collect::<Vec<_>>()})
            }
        };
        return respond_json(&mut out, 200, &listing);
    }
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if path == "/api/show" {
        shows.lock().expect("shows").push(body);
        let info = match window {
            Some(tokens) => json!({"general.architecture": "test", "test.context_length": tokens}),
            None => json!({"general.architecture": "test"}),
        };
        return respond_json(&mut out, 200, &json!({"model_info": info}));
    }
    let number = {
        let mut recorded = recorded.lock().expect("requests");
        recorded.push(body.clone());
        recorded.len()
    };
    match script(&ModelRequest {
        number,
        body: &body,
    }) {
        Reply::Json(value) => respond_json(&mut out, 200, &value),
        Reply::Status(status) => respond_json(&mut out, status, &json!({"error": "scripted"})),
        Reply::Stream(steps) => stream_reply(&mut out, wire, number, &steps),
    }
}

fn respond_json(out: &mut TcpStream, status: u16, value: &Value) -> std::io::Result<()> {
    let body = value.to_string();
    write!(
        out,
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    out.flush()
}

fn stream_reply(
    out: &mut TcpStream,
    wire: Wire,
    number: usize,
    steps: &[Step],
) -> std::io::Result<()> {
    let kind = match wire {
        Wire::Omlx => "text/event-stream",
        Wire::Ollama => "application/x-ndjson",
    };
    write!(
        out,
        "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nConnection: close\r\n\r\n"
    )?;
    let mut called_tools = false;
    for step in steps {
        let frame = match step {
            Step::Hold(gate) => {
                gate.wait(Duration::from_secs(30));
                continue;
            }
            Step::Pause(duration) => {
                std::thread::sleep(*duration);
                continue;
            }
            Step::Abort => return out.flush(),
            Step::Thinking(text) => frame(wire, json!({"thinking": text}), None),
            Step::Content(text) => frame(wire, json!({"content": text}), None),
            Step::Tools(calls) => {
                called_tools = true;
                let calls = calls
                    .iter()
                    .enumerate()
                    .map(|(index, call)| match wire {
                        Wire::Omlx => json!({
                            "index": index,
                            "id": call.id.clone().unwrap_or_else(|| format!("call-{number}-{index}")),
                            "type": "function",
                            "function": {"name": call.name, "arguments": call.arguments.to_string()},
                        }),
                        Wire::Ollama => json!({"function": {"name": call.name, "arguments": call.arguments}}),
                    })
                    .collect::<Vec<_>>();
                frame(wire, json!({"tool_calls": calls}), None)
            }
        };
        out.write_all(frame.as_bytes())?;
        out.flush()?;
    }
    let finish = frame(
        wire,
        json!({}),
        Some(if called_tools { "tool_calls" } else { "stop" }),
    );
    out.write_all(finish.as_bytes())?;
    if wire == Wire::Omlx {
        out.write_all(b"data: [DONE]\n\n")?;
    }
    out.flush()
}

/// One frame in the provider's format. `delta` holds `content`, `thinking`
/// or `tool_calls`; `finish` ends the response.
fn frame(wire: Wire, delta: Value, finish: Option<&str>) -> String {
    match wire {
        Wire::Omlx => {
            let mut fields = serde_json::Map::new();
            if let Some(text) = delta.get("content") {
                fields.insert("content".into(), text.clone());
            }
            if let Some(text) = delta.get("thinking") {
                fields.insert("reasoning_content".into(), text.clone());
            }
            if let Some(calls) = delta.get("tool_calls") {
                fields.insert("tool_calls".into(), calls.clone());
            }
            let choice = json!({"delta": fields, "finish_reason": finish});
            format!("data: {}\n\n", json!({"choices": [choice]}))
        }
        Wire::Ollama => {
            let mut message = json!({"role": "assistant", "content": delta.get("content").cloned().unwrap_or(json!(""))});
            if let Some(text) = delta.get("thinking") {
                message["thinking"] = text.clone();
            }
            if let Some(calls) = delta.get("tool_calls") {
                message["tool_calls"] = calls.clone();
            }
            let mut line = json!({"message": message, "done": finish.is_some()});
            if finish.is_some() {
                line["done_reason"] = json!("stop");
            }
            format!("{line}\n")
        }
    }
}

/// The content of the last tool message in a request.
pub fn last_tool_result(request: &Value) -> String {
    request["messages"]
        .as_array()
        .and_then(|messages| messages.iter().rev().find(|m| m["role"] == "tool"))
        .and_then(|message| message["content"].as_str())
        .unwrap_or_default()
        .to_owned()
}

/// Every user message's text, in order.
pub fn user_messages(request: &Value) -> Vec<String> {
    request["messages"]
        .as_array()
        .map(|messages| {
            messages
                .iter()
                .filter(|m| m["role"] == "user")
                .filter_map(|m| m["content"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

pub fn system_message(request: &Value) -> String {
    request["messages"][0]["content"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

pub fn tool_names(request: &Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}
