import assert from 'node:assert/strict';
import { appendFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { join } from 'node:path';

/** A loopback HTTP boundary, not a replacement RPC implementation. All SSE is controller-released. */
export class Fixture {
  requests = [];
  events = [];
  gates = [];
  errors = [];
  #listeners = new Set();
  #sockets = new Set();
  #serial = 0;

  async listen() {
    this.server = createServer((request, response) => {
      let body = Buffer.alloc(0);
      request.on('data', chunk => {
        body = Buffer.concat([body, chunk]);
        if (body.length > 8 * 1024 * 1024) request.destroy(new Error('Fixture request limit exceeded'));
      });
      request.on('error', error => { this.errors.push(String(error)); response.destroy(); this.#wake(); });
      request.on('end', () => {
        try {
          assert.equal(request.method, 'POST');
          const value = JSON.parse(body.toString('utf8'));
          if (request.url === '/control/event') {
            const event = { name: value.name, data: value.data, order: ++this.#serial };
            this.events.push(event);
            this.log('control-event', event);
            this.#json(response, { ok: true });
          } else if (request.url === '/control/gate') {
            assert.equal(value.name, 'shutdown');
            const gate = { response, name: value.name, closed: false, released: false };
            this.gates.push(gate);
            this.log('control-gate', { index: this.gates.length - 1, name: value.name });
            response.on('close', () => { gate.closed = true; this.#wake(); });
          } else if (request.url === '/chat/completions') {
            const item = {
              index: this.requests.length, method: request.method, url: request.url,
              body: value, response, opened: false, done: false, closed: false,
            };
            this.requests.push(item);
            // Intentionally never retain authorization/cookie headers, even the dummy key.
            this.log('provider-request', { index: item.index, method: item.method, url: item.url, body: value });
            response.once('close', () => {
              item.closed = true;
              this.log('provider-close', { index: item.index, done: item.done });
              this.#wake();
            });
          } else throw new Error(`Unexpected fixture URL ${request.url}`);
          this.#wake();
        } catch (error) {
          this.errors.push(String(error));
          this.log('fixture-error', { error: String(error) });
          if (!response.headersSent) response.writeHead(500);
          response.end();
          this.#wake();
        }
      });
    });
    this.server.on('connection', socket => {
      this.#sockets.add(socket);
      socket.once('close', () => this.#sockets.delete(socket));
    });
    await new Promise((resolve, reject) => {
      this.server.once('error', reject);
      this.server.listen(0, '127.0.0.1', resolve);
    });
    this.port = this.server.address().port;
    this.url = `http://127.0.0.1:${this.port}`;
  }

  reset(directory) {
    assert(this.requests.every(request => request.closed), 'Prior provider connection still live');
    assert(this.gates.every(gate => gate.closed), 'Prior control gate still live');
    this.directory = directory;
    this.requests = [];
    this.events = [];
    this.gates = [];
    this.errors = [];
    this.#serial = 0;
  }

  log(action, fields) {
    if (this.directory) appendFileSync(join(this.directory, 'fixture.jsonl'), JSON.stringify({ action, ...fields }) + '\n', { mode: 0o600 });
  }

  #json(response, value) {
    const bytes = Buffer.from(JSON.stringify(value));
    response.writeHead(200, { 'content-type': 'application/json', 'content-length': bytes.length, connection: 'close' });
    response.end(bytes);
  }

  #wake() { for (const listener of [...this.#listeners]) listener(); }

  async waitFor(check, label, timeoutMs = 10_000) {
    return new Promise((resolve, reject) => {
      let finished = false;
      const finish = (error, value) => {
        if (finished) return;
        finished = true;
        clearTimeout(timer);
        this.#listeners.delete(inspect);
        if (error) reject(error); else resolve(value);
      };
      const inspect = () => {
        try {
          if (this.errors.length) return finish(new Error(this.errors.join('\n')));
          const result = check();
          if (result) finish(null, result);
        } catch (error) { finish(error); }
      };
      const timer = setTimeout(() => finish(new Error(`Fixture deadline: ${label}`)), timeoutMs);
      this.#listeners.add(inspect);
      inspect();
    });
  }

  request(index) { return this.waitFor(() => this.requests[index], `provider request ${index}`); }
  event(name, after = 0) { return this.waitFor(() => this.events.slice(after).find(event => event.name === name), `event ${name}`); }
  gate(index = 0) { return this.waitFor(() => this.gates[index], `shutdown gate ${index}`); }
  disconnected(index = 0) { return this.waitFor(() => this.requests[index]?.closed, `provider ${index} disconnect`); }

  releaseGate(index = 0, action = 'release') {
    const gate = this.gates[index];
    assert(gate && !gate.closed && !gate.released, `No live gate ${index}`);
    gate.released = true;
    this.log('control-release', { index, action });
    this.#json(gate.response, { action });
  }

  chunk(index, delta, finishReason = null, usage) {
    const request = this.requests[index];
    assert(request && !request.closed && !request.done, `No active provider request ${index}`);
    if (!request.opened) {
      request.opened = true;
      request.response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
      request.response.flushHeaders();
    }
    const value = {
      id: `acceptance-completion-${index}`, object: 'chat.completion.chunk', created: 1, model: 'fixture',
      choices: [{ index: 0, delta, finish_reason: finishReason }], ...(usage === undefined ? {} : { usage }),
    };
    const bytes = `data: ${JSON.stringify(value)}\n\n`;
    this.log('provider-frame', { index, bytes });
    request.response.write(bytes);
  }

  text(index, value, { role = false } = {}) {
    this.chunk(index, { ...(role ? { role: 'assistant' } : {}), content: value });
  }

  tool(index = 0) {
    this.chunk(index, { role: 'assistant', tool_calls: [{
      index: 0, id: `acceptance-call-${index}`, type: 'function',
      function: { name: 'write', arguments: JSON.stringify({ path: 'must-not-exist.txt', content: 'never written' }) },
    }] });
  }

  finish(index, reason = 'stop') {
    this.chunk(index, {}, reason, { prompt_tokens: 10, completion_tokens: 4, total_tokens: 14 });
    const request = this.requests[index];
    request.done = true;
    this.log('provider-frame', { index, bytes: 'data: [DONE]\n\n' });
    request.response.end('data: [DONE]\n\n');
  }

  async close() {
    for (const socket of this.#sockets) socket.destroy();
    await new Promise(resolve => this.server.close(resolve));
  }
}
