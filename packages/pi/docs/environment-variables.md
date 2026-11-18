# Environment variables

- `PI_AGENT_DIR`: user resources and session storage directory, normally `~/.pi/agent`.
- `OPENAI_API_KEY`, `OPENAI_BASE_URL`, `OPENAI_MODEL`: OpenAI-compatible bootstrap credentials,
  endpoint, and fallback model. Provider-specific credentials remain request-time values;
  see [models](models.md).
- `PI_DOCUMENTATION_ROOT`: directory containing the installed README, docs, and examples.

Shell tools provide current `PI_SESSION_ID`, `PI_SESSION_FILE`, `PI_PROVIDER`, `PI_MODEL`, and
`PI_REASONING_LEVEL`. Unavailable values are removed rather than inherited from an earlier session.
Never print secret credential values or save them in source or session fixtures.

The Node launcher sets its documentation root to the installed package. A standalone installation
uses `share/pi-rs` beside the executable or in its parent directory.
