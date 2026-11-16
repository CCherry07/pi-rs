use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use pi_core::AbortSignal;
use rquickjs::{Context, Ctx, Exception, Function, Object, Runtime};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::{catalog::Catalog, models::ModelOperation, output::Output, source::Source};

pub(crate) const MAX_CALLS: u32 = 128;
pub(crate) const MAX_ARGUMENT_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_RESULT_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MEMORY_LIMIT: usize = 256 * 1024 * 1024;

pub(crate) struct Request {
    pub id: u32,
    pub name: String,
    pub arguments: Value,
    pub model_operation: Option<ModelOperation>,
}

pub(crate) struct Reply {
    pub id: u32,
    pub result: Result<String, String>,
    pub reservation: ReplyReservation,
}

/// Queued replies share a host-memory budget independent of the QuickJS heap.
pub(crate) struct ReplyReservation {
    bytes: usize,
    total: Arc<AtomicUsize>,
}

impl ReplyReservation {
    pub fn new(total: &Arc<AtomicUsize>, bytes: usize) -> Option<Self> {
        let previous = total.fetch_add(bytes, Ordering::Relaxed);
        if previous.saturating_add(bytes) > 32 * 1024 * 1024 {
            total.fetch_sub(bytes, Ordering::Relaxed);
            return None;
        }
        Some(Self {
            bytes,
            total: total.clone(),
        })
    }
}

impl Drop for ReplyReservation {
    fn drop(&mut self) {
        self.total.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub(crate) struct Outcome {
    pub error: Option<String>,
    pub writes: Option<Value>,
}

struct State {
    pending: HashSet<u32>,
    outcome: Option<Outcome>,
    calls: u32,
    argument_bytes: usize,
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
    pub deadline: Instant,
    pub memory_limit: usize,
}

pub(crate) fn execute(input: VmInput) -> Outcome {
    match execute_inner(&input) {
        Ok(outcome) => outcome,
        Err(error) => Outcome {
            error: Some(
                if input.signal.is_aborted() || input.parent_signal.is_aborted() {
                    "Script cancelled".into()
                } else if Instant::now() >= input.deadline {
                    "Script timed out".into()
                } else {
                    error
                },
            ),
            writes: None,
        },
    }
}

fn execute_inner(input: &VmInput) -> Result<Outcome, String> {
    let runtime = Runtime::new().map_err(|e| e.to_string())?;
    runtime.set_memory_limit(input.memory_limit);
    runtime.set_max_stack_size(256 * 1024);
    let completed = Arc::new(AtomicBool::new(false));
    let signal = input.signal.clone();
    let parent_signal = input.parent_signal.clone();
    let deadline = input.deadline;
    let is_complete = completed.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        signal.is_aborted()
            || parent_signal.is_aborted()
            || Instant::now() >= deadline
            || is_complete.load(Ordering::Relaxed)
    })));
    let context = Context::full(&runtime).map_err(|e| e.to_string())?;
    let state = Rc::new(RefCell::new(State {
        pending: HashSet::new(),
        outcome: None,
        calls: 0,
        argument_bytes: 0,
    }));
    let initialization = context.with(|ctx| {
        let call_state = state.clone();
        let requests = input.requests.clone();
        let catalog = input.catalog.clone();
        let request = Function::new(
            ctx.clone(),
            move |ctx: Ctx<'_>,
                  id: u32,
                  kind: String,
                  name: rquickjs::String<'_>,
                  args: rquickjs::String<'_>| {
                let name = bounded_string(&ctx, name, 1024)?;
                let model_operation = match kind.as_str() {
                    "tool" => None,
                    "model" => Some(ModelOperation::parse(&name).ok_or_else(|| {
                        Exception::throw_message(&ctx, "Unknown model operation")
                    })?),
                    _ => return Err(Exception::throw_message(&ctx, "Unknown host operation")),
                };
                let arguments = bounded_string(
                    &ctx,
                    args,
                    if model_operation.is_some() {
                        MAX_RESULT_BYTES
                    } else {
                        MAX_ARGUMENT_BYTES
                    },
                )?;
                let mut state = call_state.borrow_mut();
                if state.outcome.is_some() || state.calls >= MAX_CALLS {
                    return Err(Exception::throw_message(
                        &ctx,
                        "Script exceeds 128 tool calls or has completed",
                    ));
                }
                if model_operation.is_none() && !catalog.contains(&name) {
                    return Err(Exception::throw_message(
                        &ctx,
                        "Tool is outside this script's execution scope",
                    ));
                }
                state.argument_bytes = state.argument_bytes.saturating_add(arguments.len());
                if state.argument_bytes > 16 * 1024 * 1024 {
                    return Err(Exception::throw_message(
                        &ctx,
                        "Tool arguments exceed the 16 MiB script budget",
                    ));
                }
                let arguments = serde_json::from_str(&arguments).map_err(|e| {
                    Exception::throw_message(&ctx, &format!("Invalid tool arguments: {e}"))
                })?;
                state.calls += 1;
                state.pending.insert(id);
                requests
                    .send(Request {
                        id,
                        name,
                        arguments,
                        model_operation,
                    })
                    .map_err(|_| Exception::throw_message(&ctx, "Script was cancelled"))
            },
        )?;
        let output = input.output.clone();
        let budget = input.source.output_bytes;
        let emit = Function::new(
            ctx.clone(),
            move |ctx: Ctx<'_>, kind: String, value: rquickjs::String<'_>, mime: String| {
                let value = value.to_cstring()?;
                if value.len() > crate::source::MAX_OUTPUT_BYTES {
                    return Err(Exception::throw_message(
                        &ctx,
                        "Script output exceeds 16 MiB",
                    ));
                }
                output
                    .lock()
                    .map_err(|_| Exception::throw_message(&ctx, "Output state unavailable"))?
                    .append(&kind, value.as_str(), &mime, budget)
                    .map_err(|e| Exception::throw_message(&ctx, &e))
            },
        )?;
        let final_state = state.clone();
        let completed = completed.clone();
        let complete = Function::new(
            ctx.clone(),
            move |ctx: Ctx<'_>,
                  ok: bool,
                  error: rquickjs::String<'_>,
                  writes: rquickjs::String<'_>| {
                let error = bounded_string(&ctx, error, 16 * 1024)?;
                let writes = bounded_string(&ctx, writes, crate::store::MAX_STORE_BYTES)?;
                let writes = if ok {
                    Some(
                        serde_json::from_str(&writes)
                            .map_err(|_| Exception::throw_message(&ctx, "Invalid store writes"))?,
                    )
                } else {
                    None
                };
                final_state.borrow_mut().outcome = Some(Outcome {
                    error: (!ok).then_some(error),
                    writes,
                });
                completed.store(true, Ordering::Relaxed);
                Ok::<(), rquickjs::Error>(())
            },
        )?;
        let catalog = input.catalog.clone();
        let query = Function::new(
            ctx.clone(),
            move |ctx: Ctx<'_>, kind: String, args: rquickjs::String<'_>| {
                let args = bounded_string(&ctx, args, 16 * 1024)?;
                catalog
                    .query(&kind, &args)
                    .map_err(|e| Exception::throw_message(&ctx, &e))
            },
        )?;
        let prelude: Function = ctx.eval(include_str!("prelude.js"))?;
        let api: Object = prelude.call((
            request,
            emit,
            complete,
            query,
            input.catalog.declarations.as_str(),
            input.store.to_string(),
        ))?;
        let run: Function = api.get("run")?;
        run.call::<_, ()>((input.source.code.as_str(),))?;
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
            return Err("Script cancelled".into());
        }
        if Instant::now() >= input.deadline {
            return Err("Script timed out".into());
        }
        // Poll outside each small batch: a promise-only loop may execute too few
        // opcodes per job to trigger QuickJS's instruction interrupt promptly.
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
            return Err("Script is stalled: no pending tool call can resolve its promise".into());
        }
        match input.replies.recv_timeout(Duration::from_millis(10)) {
            Ok(reply) => {
                let _reservation = reply.reservation;
                state.borrow_mut().pending.remove(&reply.id);
                context
                    .with(|ctx| {
                        let api = api.clone().restore(&ctx)?;
                        let settle: Function = api.get("settle")?;
                        let (ok, payload) = match reply.result {
                            Ok(value) => (true, value),
                            Err(error) => (false, error),
                        };
                        settle.call::<_, ()>((reply.id, ok, payload))
                    })
                    .map_err(|error| context.with(|ctx| describe_error(&ctx, error)))?;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("Script host disconnected".into());
            }
        }
    }
}

fn bounded_string(
    ctx: &Ctx<'_>,
    value: rquickjs::String<'_>,
    limit: usize,
) -> rquickjs::Result<String> {
    let value = value.to_cstring()?;
    if value.len() > limit {
        return Err(Exception::throw_message(
            ctx,
            "Script value exceeds its byte limit",
        ));
    }
    Ok(value.as_str().into())
}

fn describe_error(ctx: &Ctx<'_>, error: rquickjs::Error) -> String {
    if error.is_exception() {
        let value = ctx.catch();
        if let Some(exception) = value.as_exception() {
            return exception
                .message()
                .unwrap_or_else(|| "QuickJS execution failed".into());
        }
        return "QuickJS execution failed".into();
    }
    error.to_string()
}
