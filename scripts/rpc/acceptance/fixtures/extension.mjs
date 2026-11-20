// Acceptance fixture for the real legacy and Node/NAPI extension loaders.
// Deliberately installs NO process signal handlers: the product owns shutdown.
export default function acceptance(pi) {
  const port = Number(process.env.RPC_ACCEPTANCE_CONTROL_PORT);
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('Missing control fixture port');
  const mode = process.env.RPC_ACCEPTANCE_FIXTURE;
  const post = async (path, body) => {
    const response = await fetch(`http://127.0.0.1:${port}${path}`, {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify(body), signal: AbortSignal.timeout(20_000),
    });
    if (!response.ok) throw new Error(`Fixture control HTTP ${response.status}`);
    return response.json();
  };
  const event = (name, data = {}) => post('/control/event', { name, data });
  const confirm = async (ctx, options) => {
    const answer = await ctx.ui.confirm('acceptance confirm', 'No side effects will be executed', options);
    await event('ui-released', { answer });
    return answer;
  };
  pi.on('session_start', async (_event, ctx) => {
    if (mode === 'background-ui') {
      // No fixture cancellation. A release marker must come from the real UI bridge.
      void confirm(ctx).catch(error => { console.error('acceptance fixture:', error); });
    }
    await event('fixture-ready');
  });
  pi.on('input', async (_event, ctx) => {
    if (mode === 'input-ui') {
      await confirm(ctx);
      return { action: 'handled' };
    }
  });
  pi.on('tool_call', async (_event, ctx) => {
    if (mode !== 'tool-ui') return;
    await confirm(ctx, { signal: ctx.signal });
    return { block: true, reason: 'acceptance fixture blocks writes' };
  });
  pi.on('session_shutdown', async (_event, ctx) => {
    await event('shutdown-enter');
    if (process.env.RPC_ACCEPTANCE_HOLD_SHUTDOWN === '1') {
      for (;;) {
        const value = await post('/control/gate', { name: 'shutdown' });
        if (value.action === 'release') break;
        await event('shutdown-pulse');
      }
    }
    await event('shutdown-complete');
    ctx.ui.notify('acceptance shutdown complete', 'info');
  });
}
