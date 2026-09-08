# pi-plugin-schedule

Persistent scheduled prompts, implemented entirely by the first-party Rust plugin. Every product
generation includes the `schedule` tool, `/schedule` command, and session lifecycle adapter. There
is no extra executable or operating-system scheduler installation.

Keep a primary Pi session open in the task's working directory. The plugin checks once per second
and dispatches when that session is idle with no queued input. Each attempt creates a fresh managed
isolated session with the normal providers, resources, tools, and Pi v4 persistence. It does not
inherit the parent conversation or replace the frontend's session. The prompt must be self-contained;
slash commands in prompts are not expanded by the isolated-session entry.

## Use

Ask the agent to create a task, or use the registered command:

```text
/schedule create {"name":"CI check","schedule":"every 5m","prompt":"Check the CI status for this repository. Report failures and the failing job URLs.","max_runs":12}
/schedule create {"name":"Morning summary","schedule":"0 9 * * *","timezone":"Asia/Shanghai","prompt":"Summarize the last 24 hours of Git commits in this repository."}
/schedule create {"name":"Check build","schedule":"in 30m","prompt":"Check build 42 and report its result."}
/schedule list
/schedule pause <job-id>
/schedule resume <job-id>
/schedule run_now <job-id>
/schedule history <job-id>
/schedule remove <job-id>
/schedule help
```

`create` takes a JSON object. `schedule` accepts `in Ns/Nm/Nh/Nd`, `every Ns/Nm/Nh/Nd`, an absolute
RFC3339 timestamp with offset, or a five-field cron expression. Intervals have a one-second minimum.
Cron uses `timezone`, an IANA name defaulting to `UTC`, including daylight-saving rules from Croner
and chrono-tz. A one-shot timestamp must be in the future when created.

Additional creation fields:

| Field | Default | Meaning |
| --- | --- | --- |
| `scope` | `global` | Global store or trusted `project` store |
| `paused` | `false` | Create without automatically dispatching |
| `timeout_seconds` | `600` | Deadline including launch; range 1–86400 |
| `max_runs` | unlimited | Disable after this many claimed attempts, including manual runs |
| `notify` | `true` | Show a semantic completion/error notice in the executing primary session |

Creation snapshots the current model, thinking level, and active tools. Credentials remain
request-time provider configuration and are never written into task definitions. The executing
session's current tool ceiling and model scope still apply; unavailable selections fail visibly
in the run history. Scheduled children have the `schedule` tool removed, and all isolated sessions
are forbidden from managing schedules or starting scheduler workers.

For the project store, use `"scope":"project"` on creation and a trailing `project` on other
commands, e.g. `/schedule list project` or `/schedule history <job-id> project`. Global storage is
`<agent-dir>/schedule/jobs.json`; project storage is `<cwd>/.pi/schedule/jobs.json`. The host's
existing project trust decision gates all project-store reads and writes. Each job records its
creation cwd, and both scopes dispatch/list/manage only jobs matching the currently open cwd.
Global storage does not imply execution for unopened projects.

The tool accepts the same creation fields plus `action`. Other actions are `list`, `pause`,
`resume`, `remove`, `run_now`, and `history`; mutations require `job_id`. `run_now` queues an
asynchronous attempt, even for a paused or exhausted job, and returns immediately. Repeated manual
requests before dispatch coalesce. A manual run counts towards `max_runs`; when also due it consumes
that scheduled occurrence. Pause/removal prevents future dispatch but does not interrupt an attempt
already running. History remains available after removal.

## Execution and recovery

Task data and the run ledger are updated together using a metadata file lock, a same-directory
temporary file, fsync, and atomic replacement. Each claimed attempt holds a separate OS file lock
until its result is recorded. Multiple processes and windows can share the store without claiming
the same occurrence twice. Different jobs may run concurrently across primary sessions; each
session dispatches at most one job per store per tick. Oldest due jobs take precedence.

The scheduled instant is consumed and the running record persisted before provider dispatch.
Missed windows coalesce into one attempt; recurring jobs calculate their next time from dispatch,
so long downtime does not replay a backlog. One-shots disable when claimed. Execution failures
consume an attempt rather than immediately retrying it; the next recurring occurrence remains
eligible. Normal provider retries within an attempt remain the product's existing retry policy.

The ledger records `running`, `completed`, `failed`, `aborted`, `timed_out`, or `unknown`, with
scheduled/start/finish times, child session ID, final textual output, and error. It retains the last
500 terminal attempts plus every active attempt; history returns the latest 50 matching records.
Output is capped at 64 KiB per record. Full child transcripts use the existing Pi isolated-session
storage and lifetime; they do not enter the primary resume list.

Factories perform no background work. `session_start` starts a generation-owned worker;
`session_shutdown` cancels it, aborts its child if necessary, records the outcome, and joins before
the context retires. Failed generation preparation leaves the active scheduler intact. If
cancellation cannot be confirmed within five seconds, the attempt becomes `unknown` and the job
is paused for inspection. Reloading or quitting never rewinds a consumed occurrence.

After a process crash, a running record becomes `unknown` only when its OS run lock can be acquired.
That attempt is not automatically replayed: external tool effects may already have happened. The
next recurring occurrence can still run. Locks prevent overlapping live owners, but cannot provide
exactly-once external effects after a crash. Lock files stay in place to avoid splitting locks across
different inodes.

The worker stops when all matching Pi sessions close. Reopening resumes persisted schedules under
the missed-window policy. This is an intentional pi-rs feature inspired by Hermes scheduling;
upstream Pi has no built-in cron contract to conform to.

## Embed

Construct `ScheduleOptions::new(cwd, agent_dir, project_trusted)` and register independent factories
for `SchedulePlugin` (agent lifecycle) and `ScheduleSessionPlugin` (session lifecycle). Use the
existing generation-bound `PiPluginContext` and `MultiSessionManager`; no fourth plugin lifecycle
or generic runtime scheduling policy is needed. The CLI, Node host, RPC, and desktop obtain this
wiring through `pi-sdk`.

```sh
cargo test -p pi-plugin-schedule
```
