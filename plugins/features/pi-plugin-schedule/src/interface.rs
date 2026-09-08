use async_trait::async_trait;
use pi_core::{
    Command, CommandContext, CommandError, CommandOutcome, CommandSpec, IsolatedSessionOptions,
    NoticeLevel, SessionExecutionOrigin, Tool, ToolCallId, ToolContext, ToolError,
    ToolExecutionMode, ToolResult, ToolSpec, ToolUpdateSink,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{Error, Result, ScheduleOptions, now_ms, schedule::Schedule, store::Job};

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Scope {
    #[default]
    Global,
    Project,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Create {
        name: String,
        prompt: String,
        /// 'in 30m', 'every 2h', RFC3339 timestamp, or five-field cron.
        schedule: String,
        /// IANA timezone for cron expressions; defaults to UTC.
        #[serde(default = "utc")]
        timezone: String,
        #[serde(default = "default_timeout")]
        timeout_seconds: u64,
        #[serde(default)]
        max_runs: Option<u64>,
        #[serde(default)]
        paused: bool,
        #[serde(default = "yes")]
        notify: bool,
    },
    List {},
    Pause {
        job_id: String,
    },
    Resume {
        job_id: String,
    },
    Remove {
        job_id: String,
    },
    RunNow {
        job_id: String,
    },
    History {
        #[serde(default)]
        job_id: Option<String>,
    },
}

#[derive(Deserialize)]
struct Request {
    #[serde(default)]
    scope: Scope,
    #[serde(flatten)]
    action: Action,
}

fn utc() -> String {
    "UTC".into()
}
fn default_timeout() -> u64 {
    600
}
fn yes() -> bool {
    true
}

impl Request {
    fn parse(value: Value) -> Result<Self> {
        let request: Self = serde_json::from_value(value)?;
        if let Action::Create {
            name,
            prompt,
            schedule,
            timezone,
            timeout_seconds,
            max_runs,
            ..
        } = &request.action
        {
            if name.trim().is_empty()
                || name.len() > 200
                || prompt.trim().is_empty()
                || prompt.len() > 64 * 1024
            {
                return Err(Error::Invalid(
                    "name must be 1–200 bytes and prompt 1–65536 bytes".into(),
                ));
            }
            if !(1..=86_400).contains(timeout_seconds) || *max_runs == Some(0) {
                return Err(Error::Invalid(
                    "timeout_seconds must be 1–86400; max_runs must be positive".into(),
                ));
            }
            Schedule::parse(schedule, timezone, now_ms())?;
        }
        Ok(request)
    }
}

pub(crate) struct ScheduleTool {
    options: ScheduleOptions,
}
pub(crate) struct ScheduleCommand {
    options: ScheduleOptions,
}

impl ScheduleTool {
    pub fn new(options: ScheduleOptions) -> Self {
        Self { options }
    }
}

impl ScheduleCommand {
    pub fn new(options: ScheduleOptions) -> Self {
        Self { options }
    }
}

#[async_trait]
impl Tool for ScheduleTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "schedule".into(), label: "Scheduled tasks".into(),
            description: "Manage persistent scheduled prompts. Jobs run in fresh isolated sessions while a primary Pi session in the same cwd is open and idle. Actions: create, list, pause, resume, remove, run_now, history. run_now queues an asynchronous run. Scope defaults to global; project requires trust. Creating a job snapshots the current model, thinking and active tools.".into(),
            parameters: json!({
                "type": "object", "additionalProperties": false,
                "required": ["action"],
                "properties": {
                    "action": {"type":"string", "enum":["create","list","pause","resume","remove","run_now","history"]},
                    "scope": {"type":"string", "enum":["global","project"]},
                    "job_id": {"type":"string"},
                    "name": {"type":"string"},
                    "prompt": {"type":"string"},
                    "schedule": {"type":"string", "description":"in 30m, every 2h, RFC3339, or five-field cron"},
                    "timezone": {"type":"string", "description":"IANA timezone; default UTC"},
                    "timeout_seconds": {"type":"integer", "minimum":1, "maximum":86400},
                    "max_runs": {"type":"integer", "minimum":1},
                    "paused": {"type":"boolean"},
                    "notify": {"type":"boolean"}
                }
            }),
            execution_mode: ToolExecutionMode::Sequential,
            prompt_snippet: Some("Schedule future or recurring work with the schedule tool.".into()),
            prompt_guidelines: vec!["Use self-contained prompts: scheduled sessions have no parent conversation history. Specify an IANA timezone for wall-clock cron times. Pi must remain running in the job's working directory. Missed triggers coalesce; interrupted attempts are not automatically replayed.".into()],
        }
    }

    fn validate_arguments(&self, input: &Value) -> std::result::Result<(), ToolError> {
        Request::parse(input.clone())
            .map(|_| ())
            .map_err(|error| ToolError::InvalidArguments(error.to_string()))
    }

    async fn execute(
        &self,
        context: ToolContext,
        _: ToolCallId,
        input: Value,
        _: ToolUpdateSink,
    ) -> std::result::Result<ToolResult, ToolError> {
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        ensure_primary(context.session.execution_origin()?)
            .map_err(|error| ToolError::Execution(error.to_string()))?;
        let request = Request::parse(input)
            .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
        let selections = if matches!(request.action, Action::Create { .. }) {
            IsolatedSessionOptions {
                model: context.models.selection()?,
                thinking_level: context.models.thinking_level()?,
                active_tools: Some(context.session.active_tools()?),
            }
        } else {
            IsolatedSessionOptions::default()
        };
        let result = manage(self.options.clone(), request, selections)
            .await
            .map_err(|error| ToolError::Execution(error.to_string()))?;
        let mut output =
            ToolResult::text(serde_json::to_string_pretty(&result).unwrap_or_default());
        output.details = Some(result);
        Ok(output)
    }
}

const HELP: &str = "Scheduled tasks run while Pi is open in this working directory.\n\
  /schedule list [global|project]\n\
  /schedule history [job-id] [global|project]\n\
  /schedule pause|resume|remove|run_now <job-id> [global|project]\n\
  /schedule create {\"name\":\"CI\",\"schedule\":\"every 5m\",\"prompt\":\"Check CI status\"}\n\
Create accepts timezone (default UTC), scope (global/project), timeout_seconds, max_runs, paused, notify.\n\
Schedules: in 30m, every 2h, RFC3339, or five-field cron. Results: /schedule history.";

#[async_trait]
impl Command for ScheduleCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "schedule".into(),
            description: "Manage persistent scheduled tasks".into(),
            argument_hint: Some("[list|create|pause|resume|remove|run_now|history|help]".into()),
        }
    }

    async fn execute(
        &self,
        context: CommandContext,
        arguments: String,
    ) -> std::result::Result<CommandOutcome, CommandError> {
        context
            .signal()
            .check()
            .map_err(|_| CommandError::Aborted)?;
        ensure_primary(context.session.execution_origin()?)
            .map_err(|error| CommandError::Execution(error.to_string()))?;
        if arguments.trim() == "help" {
            context.ui.notify(NoticeLevel::Info, HELP)?;
            return Ok(CommandOutcome::Handled);
        }
        let request = parse_command(&arguments)
            .map_err(|error| CommandError::InvalidArguments(error.to_string()))?;
        let selections = if matches!(request.action, Action::Create { .. }) {
            IsolatedSessionOptions {
                model: context.models.selection()?,
                thinking_level: context.models.thinking_level()?,
                active_tools: Some(context.session.active_tools()?),
            }
        } else {
            IsolatedSessionOptions::default()
        };
        let result = manage(self.options.clone(), request, selections)
            .await
            .map_err(|error| CommandError::Execution(error.to_string()))?;
        context.ui.notify(
            NoticeLevel::Info,
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        )?;
        Ok(CommandOutcome::Handled)
    }
}

fn ensure_primary(origin: SessionExecutionOrigin) -> Result<()> {
    if origin != SessionExecutionOrigin::User {
        return Err(Error::Invalid(
            "isolated sessions cannot manage schedules".into(),
        ));
    }
    Ok(())
}

fn parse_command(arguments: &str) -> Result<Request> {
    let arguments = arguments.trim();
    if let Some(json) = arguments.strip_prefix("create ") {
        let mut value: Value = serde_json::from_str(json)?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| Error::Invalid("create expects a JSON object".into()))?;
        object.insert("action".into(), json!("create"));
        return Request::parse(value);
    }
    let mut words: Vec<&str> = arguments.split_whitespace().collect();
    let scope = if words.last() == Some(&"project") {
        words.pop();
        "project"
    } else {
        if words.last() == Some(&"global") {
            words.pop();
        }
        "global"
    };
    let action = words.first().copied().unwrap_or("list");
    let value = match (action, words.get(1), words.len()) {
        ("list", None, _) => json!({"action": "list", "scope": scope}),
        ("history", id, 1..=2) => json!({"action": "history", "job_id": id, "scope": scope}),
        ("pause" | "resume" | "remove" | "run_now", Some(id), 2) => {
            json!({"action": action, "job_id": id, "scope": scope})
        }
        _ => return Err(Error::Invalid(HELP.into())),
    };
    Request::parse(value)
}

async fn manage(
    options: ScheduleOptions,
    request: Request,
    selections: IsolatedSessionOptions,
) -> Result<Value> {
    tokio::task::spawn_blocking(move || {
        let store = options.store(request.scope)?;
        let now = now_ms();
        match request.action {
            Action::Create {
                name,
                prompt,
                schedule,
                timezone,
                timeout_seconds,
                max_runs,
                paused,
                notify,
            } => {
                let schedule = Schedule::parse(&schedule, &timezone, now)?;
                let next_run_at = Some(schedule.next(now)?);
                let job = Job {
                    id: uuid::Uuid::now_v7().to_string(),
                    name,
                    cwd: options.cwd,
                    prompt,
                    schedule,
                    options: selections,
                    enabled: !paused,
                    next_run_at,
                    manual_requested: false,
                    timeout_seconds,
                    max_runs,
                    runs: 0,
                    notify,
                };
                Ok(serde_json::to_value(store.create(job)?)?)
            }
            Action::List {} => Ok(serde_json::to_value(store.list(&options.cwd)?)?),
            Action::History { job_id } => Ok(serde_json::to_value(
                store.history(&options.cwd, job_id.as_deref())?,
            )?),
            Action::Pause { job_id } => Ok(serde_json::to_value(store.change(
                &options.cwd,
                &job_id,
                "pause",
                now,
            )?)?),
            Action::Resume { job_id } => Ok(serde_json::to_value(store.change(
                &options.cwd,
                &job_id,
                "resume",
                now,
            )?)?),
            Action::Remove { job_id } => Ok(serde_json::to_value(store.change(
                &options.cwd,
                &job_id,
                "remove",
                now,
            )?)?),
            Action::RunNow { job_id } => Ok(serde_json::to_value(store.change(
                &options.cwd,
                &job_id,
                "run_now",
                now,
            )?)?),
        }
    })
    .await
    .map_err(|error| Error::Invalid(format!("schedule storage worker failed: {error}")))?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_and_tool_share_validation() {
        assert!(parse_command(r#"create {"name":"检查","schedule":"every 5m","prompt":"检查服务","scope":"project"}"#).is_ok());
        assert!(parse_command("list project").is_ok());
        assert!(parse_command("pause abc project").is_ok());
        assert!(parse_command("history").is_ok());
        assert!(parse_command("pause").is_err());
        assert!(Request::parse(json!({"action":"create","name":"x","prompt":"x","schedule":"in 1m","timeout_seconds":0})).is_err());
        assert!(Request::parse(json!({"action":"list","unrecognized":true})).is_err());
    }
}
