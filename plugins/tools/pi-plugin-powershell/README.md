# pi-plugin-powershell

Optional Windows PowerShell tool for a Pi runtime. Register `PowerShellPlugin`, or
`ConfiguredPowerShellPlugin::new(PowerShellToolOptions { expose_session_environment: false })`
to omit session and model metadata from the child environment.

The tool prefers PowerShell 7 and falls back to Windows PowerShell. It accepts
`command` and an optional timeout in seconds, with the current Pi process arguments,
UTF-8 output setup, and Windows PowerShell native-stderr exit-status handling.

Shell execution, cancellation, output capture and truncation are shared with Bash.
Programmatic callers receive combined output, a truncation flag, exit code and wall
time; output above 1 MiB retains its beginning and end and includes a full-output
file path. Model-visible output retains the 2,000-line / 50 KiB display limits.
