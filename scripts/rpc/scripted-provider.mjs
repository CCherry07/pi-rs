// Deterministic local HTTP fixture; no external model is called.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';

export async function createScriptedProvider() {
  const requests = [], waiting = new Set(), releases = new Map(), partReleases = new Map();
  let turns = [];
  const server = createServer((request, response) => {
    let body = '';
    request.setEncoding('utf8');
    request.on('data', chunk => {
      body += chunk;
      if (body.length > 2 * 1024 * 1024) request.destroy(new Error('Fixture request too large'));
    });
    request.on('end', async () => {
      let value;
      try { value = JSON.parse(body); } catch { response.writeHead(400).end(); return; }
      // Never retain authorization/cookie headers, even with the fixture dummy key.
      const index = requests.length;
      requests.push({ method: request.method, url: request.url, body: value });
      const plan = turns[index];
      if (request.method !== 'POST' || !request.url.endsWith('/chat/completions') || !plan) {
        response.writeHead(500).end(JSON.stringify({ error: { message: 'Unexpected fixture request' } }));
        return;
      }
      let release;
      const gate = new Promise(resolve => { release = resolve; });
      releases.set(index, release);
      response.once('close', () => {
        releases.delete(index); release();
        for (const [key, resume] of partReleases) if (key.startsWith(`${index}:`)) { partReleases.delete(key); resume(); }
      });
      response.once('finish', () => releases.delete(index));
      for (const waiter of [...waiting]) if (requests.length >= waiter.count) { waiting.delete(waiter); waiter.resolve(); }
      const chunk = (delta, finish_reason = null, usage) => ({
        id: 'fixture-completion', object: 'chat.completion.chunk', created: 1, model: 'fixture',
        choices: [{ index: 0, delta, finish_reason }], ...(usage ? { usage } : {}),
      });
      const send = event => { if (!response.destroyed) response.write(`data: ${JSON.stringify(event)}\n\n`); };
      if (plan.hold === 'before') await gate;
      if (response.destroyed) return;
      response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
      send(chunk({ role: 'assistant' }));
      if (plan.tool) {
        send(chunk({ tool_calls: [{ index: 0, id: 'fixture-call', type: 'function', function: { name: plan.tool, arguments: '{"value":"hello"}' } }] }));
      } else {
        for (const [partIndex, part] of (plan.parts ?? ['fixture ', 'answer']).entries()) {
          // Install the gate before writing. The parent releases it only after
          // consuming this actual text_delta from the product's stdout.
          const acknowledged = plan.controlled ? new Promise(resolve => { partReleases.set(`${index}:${partIndex}`, resolve); }) : undefined;
          send(chunk({ content: part }));
          if (acknowledged) await acknowledged;
          if (response.destroyed) return;
        }
      }
      if (plan.hold === 'after') await gate;
      if (response.destroyed) return;
      send(chunk({}, plan.tool ? 'tool_calls' : 'stop', { prompt_tokens: 10, completion_tokens: 4, total_tokens: 14 }));
      response.end('data: [DONE]\n\n');
    });
    request.on('error', () => response.destroy());
  });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
  return {
    url: `http://127.0.0.1:${server.address().port}`,
    requests,
    reset(plans = [{}]) { assert.equal(releases.size + partReleases.size, 0, 'Previous fixture request is still active'); requests.length = 0; turns = plans; },
    waitForRequest(count) {
      if (requests.length >= count) return Promise.resolve();
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => { waiting.delete(waiter); reject(new Error(`Fixture request ${count} timed out`)); }, 15000);
        const waiter = { count, resolve: () => { clearTimeout(timer); resolve(); } };
        waiting.add(waiter);
      });
    },
    release(index = 0) { const release = releases.get(index); assert(release, `No held request ${index}`); release(); },
    releasePart(index, part) { const key = `${index}:${part}`; const resume = partReleases.get(key); assert(resume, `No held content ${key}`); partReleases.delete(key); resume(); },
    async close() {
      server.closeAllConnections();
      await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
    },
  };
}
