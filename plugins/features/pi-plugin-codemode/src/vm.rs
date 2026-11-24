use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use pi_core::AbortSignal;
use rquickjs::{Context, Ctx, Exception, Function, Object, Runtime, context::EvalOptions};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::{catalog::Catalog, models::ModelOperation, output::Output, source::Source};

pub(crate) const MEMORY_LIMIT: usize = 256 * 1024 * 1024;
/// QuickJS's finite native-stack guard. The VM runs on an explicitly larger OS thread stack.
pub(crate) const STACK_LIMIT: usize = 1024 * 1024;
/// Native stack reserved for each bounded codemode worker thread.
pub(crate) const WORKER_STACK_SIZE: usize = 8 * 1024 * 1024;
/// Maximum object/array nesting accepted for one caller-controlled bridge value.
pub(crate) const MAX_BRIDGE_VALUE_DEPTH: usize = 512;
/// Fixed headroom for host envelopes around an accepted value.
pub(crate) const MAX_BRIDGE_DOCUMENT_DEPTH: usize = MAX_BRIDGE_VALUE_DEPTH + 32;

pub(crate) struct Request {
    pub id: u32,
    pub name: String,
    pub arguments: Value,
    pub arguments_json: Option<String>,
    pub model_operation: Option<ModelOperation>,
    pub discovery: bool,
}

pub(crate) struct Reply {
    pub id: u32,
    /// None represents JavaScript undefined, distinct from JSON null.
    pub result: Result<Option<String>, String>,
}

#[derive(Default)]
pub(crate) struct Outcome {
    /// Already formatted script/timeout/sandbox error, without the call summary.
    pub error: Option<String>,
    pub writes: Option<Value>,
}

struct State {
    pending: HashSet<u32>,
    outcome: Option<Outcome>,
}

pub(crate) struct VmInput {
    pub source: Source,
    pub catalog: Arc<Catalog>,
    pub store: Value,
    pub signal: AbortSignal,
    pub parent_signal: AbortSignal,
    pub output: Arc<Mutex<Output>>,
    pub requests: UnboundedSender<Request>,
    pub replies: mpsc::Receiver<Reply>,
    pub deadline: Option<Instant>,
    pub memory_limit: usize,
    pub models: bool,
}

fn expired(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

pub(crate) fn execute(input: VmInput) -> Outcome {
    match execute_inner(&input) {
        Ok(outcome) => outcome,
        Err(error) => Outcome {
            error: Some(
                if input.signal.is_aborted() || input.parent_signal.is_aborted() {
                    "Script aborted: Execution aborted".into()
                } else if expired(input.deadline) {
                    timeout_error(input.source.timeout)
                } else {
                    error
                },
            ),
            writes: None,
        },
    }
}

pub(crate) fn timeout_error(timeout: Option<Duration>) -> String {
    format!(
        "Script timed out: Execution timed out after {} ms",
        timeout.unwrap_or_default().as_millis()
    )
}

fn optional_string(value: &rquickjs::Value<'_>) -> rquickjs::Result<Option<String>> {
    if value.is_undefined() {
        return Ok(None);
    }
    value
        .as_string()
        .ok_or_else(|| rquickjs::Error::new_from_js(value.type_name(), "string"))?
        .to_string()
        .map(Some)
}

fn parse_writes(raw: &str) -> Result<Value, String> {
    // The writes envelope is shallow. Each JSON-text value receives the complete logical value
    // budget independently, so bridge and session wrappers cannot reduce the accepted depth.
    let entries: Vec<Vec<String>> = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    let mut set = serde_json::Map::new();
    let mut delete = Vec::new();
    for entry in entries {
        match entry.as_slice() {
            [key] => delete.push(key.clone()),
            [key, value] => {
                set.insert(
                    key.clone(),
                    pi_core::json::from_str_with_max_depth(value, MAX_BRIDGE_VALUE_DEPTH)
                        .map_err(|error| error.to_string())?,
                );
            }
            _ => return Err("Invalid store writes".into()),
        }
    }
    Ok(json!({"set":set,"delete":delete}))
}

fn script_error(raw: &str) -> String {
    let error: Value = serde_json::from_str(raw).unwrap_or_default();
    error["stack"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| {
            format!(
                "{}: {}",
                error["name"].as_str().unwrap_or("Error"),
                error["message"]
                    .as_str()
                    .unwrap_or("The script threw a value that cannot be described")
            )
        })
}

fn execute_inner(input: &VmInput) -> Result<Outcome, String> {
    let runtime = Runtime::new().map_err(|e| e.to_string())?;
    runtime.set_memory_limit(input.memory_limit);
    runtime.set_max_stack_size(STACK_LIMIT);
    let completed = Arc::new(AtomicBool::new(false));
    let signal = input.signal.clone();
    let parent_signal = input.parent_signal.clone();
    let deadline = input.deadline;
    let is_complete = completed.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        signal.is_aborted()
            || parent_signal.is_aborted()
            || expired(deadline)
            || is_complete.load(Ordering::Relaxed)
    })));
    let context = Context::full(&runtime).map_err(|e| e.to_string())?;
    let state = Rc::new(RefCell::new(State {
        pending: HashSet::new(),
        outcome: None,
    }));
    let initialization = context.with(|ctx| {
        let state = state.clone();
        let requests = input.requests.clone();
        let catalog = input.catalog.clone();
        let output = input.output.clone();
        let complete = completed.clone();
        let bridge = Function::new(
            ctx.clone(),
            move |ctx: Ctx<'_>,
                  kind: String,
                  a: rquickjs::Value<'_>,
                  b: rquickjs::Value<'_>,
                  c: rquickjs::Value<'_>| {
                // The first terminal event wins. A caught exit/output-limit
                // exception can execute more JS before an instruction interrupt.
                // Such JS must never start a post-settlement host operation.
                if state.borrow().outcome.is_some() {
                    return Ok(());
                }
                let result = (|| -> rquickjs::Result<()> {
                    let host_error = |message: &str| Exception::throw_message(&ctx, message);
                    match kind.as_str() {
                        "call" | "global" => {
                            let id =
                                a.as_int().ok_or_else(|| host_error("Invalid call id"))? as u32;
                            let name = optional_string(&b)?
                                .ok_or_else(|| host_error("Missing call name"))?;
                            let arguments_json = optional_string(&c)?;
                            let arguments = arguments_json
                                .as_deref()
                                .map(|json| {
                                    pi_core::json::from_str_with_max_depth(
                                        json,
                                        MAX_BRIDGE_VALUE_DEPTH,
                                    )
                                })
                                .transpose()
                                .map_err(|e| host_error(&format!("Invalid tool arguments: {e}")))?
                                .unwrap_or(Value::Null);
                            let model_operation = if kind == "global" {
                                name.strip_prefix("models.").and_then(ModelOperation::parse)
                            } else {
                                None
                            };
                            let discovery = kind == "global" && model_operation.is_none();
                            if kind == "call" && !catalog.contains(&name) {
                                return Err(host_error(
                                    "Tool is outside this script's execution scope",
                                ));
                            }
                            state.borrow_mut().pending.insert(id);
                            requests
                                .send(Request {
                                    id,
                                    name,
                                    arguments,
                                    arguments_json,
                                    model_operation,
                                    discovery,
                                })
                                .map_err(|_| host_error("Script was cancelled"))?;
                        }
                        "output" => {
                            let kind = optional_string(&a)?.unwrap_or_default();
                            let value = optional_string(&b)?.unwrap_or_default();
                            let mime = optional_string(&c)?.unwrap_or_default();
                            output
                                .lock()
                                .map_err(|_| host_error("Output state unavailable"))?
                                .append(&kind, &value, &mime);
                        }
                        "done" => {
                            let ok = a.as_bool().unwrap_or(false);
                            let payload = optional_string(&b)?;
                            let writes = if ok {
                                Some(
                                    parse_writes(&optional_string(&c)?.unwrap_or_default())
                                        .map_err(|error| host_error(&error))?,
                                )
                            } else {
                                None
                            };
                            if ok && let Some(payload) = payload.as_deref() {
                                // Returned values cross JSON first, unlike text(). JSON strings
                                // are unquoted for display; other JSON stays compact verbatim.
                                let rendered = if payload.starts_with('"') {
                                    serde_json::from_str::<String>(payload)
                                        .map_err(|e| host_error(&e.to_string()))?
                                } else {
                                    // The immutable prelude produced this JSON. Do
                                    // not impose serde's container depth cap merely
                                    // to display an already-serialized return value.
                                    payload.into()
                                };
                                output
                                    .lock()
                                    .map_err(|_| host_error("Output state unavailable"))?
                                    .append("text", &rendered, "");
                            }
                            state.borrow_mut().outcome = Some(Outcome {
                                error: (!ok)
                                    .then(|| script_error(payload.as_deref().unwrap_or("{}"))),
                                writes,
                            });
                            complete.store(true, Ordering::Relaxed);
                        }
                        _ => return Err(host_error("Unknown host operation")),
                    }
                    Ok(())
                })();
                // done() has already marked the prelude finished. Throwing a
                // decoding error back into its ignored continuation would leave
                // no outcome and no possible future progress (with no deadline).
                // Treat every malformed bridge message as terminal, as the
                // oracle host does, including UTF-16 values Rust cannot carry.
                if let Err(error) = result {
                    state.borrow_mut().outcome = Some(Outcome {
                        error: Some(format!(
                            "Script sandbox failed: Sandbox bridge broken: {}",
                            describe_error(&ctx, error)
                        )),
                        writes: None,
                    });
                    complete.store(true, Ordering::Relaxed);
                }
                Ok::<(), rquickjs::Error>(())
            },
        )?;
        let mut options = EvalOptions::default();
        options.filename = Some("codemode-prelude.js".into());
        let prelude: Function = ctx.eval_with_options(include_str!("prelude.js"), options)?;
        let globals: Vec<_> = [
            "searchTools",
            "describeTool",
            "describeNamespace",
            "models.getModelsOfType",
            "models.getAvailableOfType",
            "models.getModelOfType",
            "models.classify",
            "models.generateImages",
        ]
        .into_iter()
        .filter(|name| input.models || !name.starts_with("models."))
        .map(|name| json!({"name":name,"spread":true}))
        .collect();
        let store: serde_json::Map<String, Value> = input
            .store
            .as_object()
            .into_iter()
            .flatten()
            .map(|(key, value)| {
                pi_core::json::to_string_with_max_depth(value, MAX_BRIDGE_VALUE_DEPTH)
                    .map(|json| (key.clone(), Value::String(json)))
                    .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))
            })
            .collect::<Result<_, _>>()?;
        let api: Object = prelude.call((
            bridge,
            input.catalog.declarations.as_str(),
            serde_json::to_string(&globals).unwrap(),
            serde_json::to_string(&store).unwrap(),
        ))?;
        let run: Function = api.get("run")?;
        let mut options = EvalOptions::default();
        options.strict = false;
        options.filename = Some("codemode.js".into());
        let script: Function = ctx.eval_with_options(
            format!("(async (tools, console) => {{{}\n}})", input.source.code),
            options,
        )?;
        run.call::<_, ()>((script,))?;
        Ok::<_, rquickjs::Error>(rquickjs::Persistent::save(&ctx, api))
    });
    let api = match initialization {
        Ok(api) => api,
        Err(error) => {
            if let Some(outcome) = state.borrow_mut().outcome.take() {
                return Ok(outcome);
            }
            return Err(context.with(|ctx| describe_error(&ctx, error)));
        }
    };
    loop {
        if let Some(outcome) = state.borrow_mut().outcome.take() {
            return Ok(outcome);
        }
        if input.signal.is_aborted() || input.parent_signal.is_aborted() {
            return Err("Script aborted: Execution aborted".into());
        }
        if expired(input.deadline) {
            return Err(timeout_error(input.source.timeout));
        }
        for _ in 0..50 {
            match runtime.execute_pending_job() {
                Ok(false) => break,
                Ok(true) => {}
                Err(error) => {
                    if let Some(outcome) = state.borrow_mut().outcome.take() {
                        return Ok(outcome);
                    }
                    return Err(error
                        .0
                        .with(|ctx| describe_error(&ctx, rquickjs::Error::Exception)));
                }
            }
            if state.borrow().outcome.is_some() {
                break;
            }
        }
        if state.borrow().outcome.is_some() || runtime.is_job_pending() {
            continue;
        }
        if state.borrow().pending.is_empty() {
            context
                .with(|ctx| {
                    let api = api.clone().restore(&ctx)?;
                    let stalled: Function = api.get("stalled")?;
                    stalled.call::<_, bool>(())
                })
                .map_err(|error| context.with(|ctx| describe_error(&ctx, error)))?;
            continue;
        }
        match input.replies.recv_timeout(Duration::from_millis(10)) {
            Ok(reply) => {
                state.borrow_mut().pending.remove(&reply.id);
                context
                    .with(|ctx| {
                        let api = api.clone().restore(&ctx)?;
                        let settle: Function = api.get("settle")?;
                        let (ok, payload) = match reply.result {
                            Ok(value) => (true, value),
                            Err(error) => (false, Some(error)),
                        };
                        settle.call::<_, ()>((reply.id, ok, payload))
                    })
                    .map_err(|error| context.with(|ctx| describe_error(&ctx, error)))?;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("Script sandbox failed: Script host disconnected".into());
            }
        }
    }
}

fn describe_error(ctx: &Ctx<'_>, error: rquickjs::Error) -> String {
    if error.is_exception() {
        let value = ctx.catch();
        if let Some(exception) = value.as_exception() {
            let name: String = exception.get("name").unwrap_or_else(|_| "Error".into());
            let message = exception.message().unwrap_or_default();
            let stack = exception.stack().unwrap_or_default();
            let frames = stack
                .lines()
                .filter(|line| !line.trim().is_empty() && !line.contains("codemode-prelude.js"))
                .collect::<Vec<_>>();
            let head = if message.is_empty() {
                name
            } else {
                format!("{name}: {message}")
            };
            return std::iter::once(head.as_str())
                .chain(frames)
                .collect::<Vec<_>>()
                .join("\n");
        }
        return "Error: QuickJS execution failed".into();
    }
    format!("Script sandbox failed: {error}")
}
