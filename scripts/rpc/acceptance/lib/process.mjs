import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { appendFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { StringDecoder } from 'node:string_decoder';

const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');

/** One owned product process; never accepts a PID supplied by a caller. */
export class OwnedRpc {
  records = [];
  parseErrors = [];
  transportErrors = [];
  bytes = 0;
  closed = false;
  exited = false;
  paused = false;
  timedOut = false;
  #buffer = '';
  #decoder = new StringDecoder('utf8');
  #utf8 = new TextDecoder('utf-8', { fatal: true });
  #trailingBytes = 0;
  #listeners = new Set();
  #nextId = 0;
  #seq = 0;
  #started = process.hrtime.bigint();
  #pauseAt = Infinity;

  constructor({ executable, args, cwd, env, directory, timeoutMs = 20_000, onSpawn }) {
    this.directory = directory;
    this.timeoutMs = timeoutMs;
    mkdirSync(directory, { recursive: true, mode: 0o700 });
    for (const file of ['stdout.raw', 'stderr.raw', 'stdin.raw', 'controller.jsonl']) {
      writeFileSync(join(directory, file), '', { mode: 0o600 });
    }
    this.child = spawn(executable, args, {
      cwd, env, stdio: ['pipe', 'pipe', 'pipe'], detached: process.platform !== 'win32',
    });
    this.pid = this.child.pid;
    this.pgid = process.platform === 'win32' ? null : this.pid;
    const identity = { pid: this.pid, pgid: this.pgid, executable, args, cwd, env };
    writeFileSync(join(directory, 'launch.json'), JSON.stringify(identity, null, 2), { mode: 0o600 });
    this.note('spawn', identity);
    // The outer watchdog receives only a PID actually returned by our spawn.
    if (this.pid) onSpawn?.({ pid: this.pid, pgid: this.pgid });
    this.child.stdout.on('data', bytes => {
      appendFileSync(join(directory, 'stdout.raw'), bytes);
      this.bytes += bytes.length;
      const lastLF = bytes.lastIndexOf(10);
      this.#trailingBytes = lastLF < 0 ? this.#trailingBytes + bytes.length : bytes.length - lastLF - 1;
      try { this.#utf8.decode(bytes, { stream: true }); }
      catch (error) { this.parseErrors.push({ afterRecord: this.records.length, error: `Invalid UTF-8: ${error.message}` }); }
      this.#buffer += this.#decoder.write(bytes);
      for (let end; (end = this.#buffer.indexOf('\n')) >= 0;) {
        const line = this.#buffer.slice(0, end);
        this.#buffer = this.#buffer.slice(end + 1);
        try { this.records.push(JSON.parse(line)); }
        catch (error) { this.parseErrors.push({ afterRecord: this.records.length, line, error: String(error) }); }
      }
      if (this.bytes >= this.#pauseAt) {
        this.#pauseAt = Infinity;
        this.child.stdout.pause();
        this.paused = true;
        this.note('stdout-paused', { bytes: this.bytes, completeRecords: this.records.length });
      }
      if (this.bytes > 64 * 1024 * 1024) {
        this.transportErrors.push('64 MiB stdout safety limit exceeded');
        this.killOwnedGroup('output-limit');
      }
      this.#wake();
    });
    this.child.stdout.on('end', () => {
      try { this.#utf8.decode(); }
      catch (error) { this.parseErrors.push({ afterRecord: this.records.length, error: `Incomplete UTF-8: ${error.message}` }); }
      this.#buffer += this.#decoder.end();
      this.stdoutEnded = this.note('stdout-end', { bytes: this.bytes });
      this.#wake();
    });
    this.child.stderr.on('data', bytes => appendFileSync(join(directory, 'stderr.raw'), bytes));
    this.child.stderr.on('end', () => { this.note('stderr-end'); });
    this.child.stdin.on('error', error => {
      this.transportErrors.push(`stdin: ${error.message}`);
      this.note('stdin-error', { error: error.message });
      this.#wake();
    });
    this.child.on('error', error => {
      this.transportErrors.push(`spawn: ${error.message}`);
      this.note('process-error', { error: error.message });
      this.#wake();
    });
    this.child.on('exit', (code, signal) => {
      this.exited = true;
      this.exit = { code, signal, observed: this.note('process-exit', { code, signal }) };
      this.#wake();
    });
    this.completed = new Promise(resolve => {
      this.child.on('close', (code, signal) => {
        clearTimeout(this.deadline);
        this.closed = true;
        this.result = {
          code, signal, pid: this.pid, pgid: this.pgid, timedOut: this.timedOut,
          exit: this.exit, close: this.note('process-close', { code, signal }),
          stdoutEnd: this.stdoutEnded ?? null, stdoutBytes: this.bytes,
          trailingBytes: this.trailingBytes, parseErrors: this.parseErrors,
          transportErrors: this.transportErrors,
        };
        writeFileSync(join(directory, 'exit.json'), JSON.stringify(this.result, null, 2), { mode: 0o600 });
        this.#wake();
        resolve(this.result);
      });
    });
    this.deadline = setTimeout(() => {
      this.timedOut = true;
      this.note('hard-deadline', { timeoutMs });
      this.resumeOutput();
      this.killOwnedGroup('hard-deadline');
    }, timeoutMs);
  }

  get trailingBytes() { return this.#trailingBytes; }
  get nextId() { return `acceptance-${++this.#nextId}`; }

  note(action, fields = {}) {
    const value = { seq: ++this.#seq, elapsedNs: String(process.hrtime.bigint() - this.#started), action, ...fields };
    appendFileSync(join(this.directory, 'controller.jsonl'), JSON.stringify(value) + '\n');
    return value;
  }

  #wake() { for (const listener of [...this.#listeners]) listener(); }

  async waitUntil(check, { label = 'condition', timeoutMs = this.timeoutMs } = {}) {
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
          const value = check();
          if (value) return finish(null, value);
          if (this.closed) finish(new Error(`Process closed before ${label}: ${this.result.code}/${this.result.signal}`));
        } catch (error) { finish(error); }
      };
      const timer = setTimeout(() => finish(new Error(`Deadline waiting for ${label}`)), timeoutMs);
      this.#listeners.add(inspect);
      inspect();
    });
  }

  waitFor(predicate, { after = 0, label = 'RPC record', timeoutMs } = {}) {
    return this.waitUntil(() => this.records.slice(after).find(predicate), { label, timeoutMs });
  }

  response(id) {
    return this.waitFor(record => record?.type === 'response' && record.id === id, { label: `response ${id}` });
  }

  async sendRaw(bytes) {
    assert(!this.closed && !this.child.stdin.writableEnded, 'stdin is already closed');
    bytes = Buffer.from(bytes);
    appendFileSync(join(this.directory, 'stdin.raw'), bytes);
    this.note('stdin-write', { bytes: bytes.length, sha256: sha256(bytes) });
    await new Promise((resolve, reject) => this.child.stdin.write(bytes, error => error ? reject(error) : resolve()));
  }

  async send(record) { await this.sendRaw(JSON.stringify(record) + '\n'); }

  async command(type, fields = {}) {
    const id = this.nextId;
    await this.send({ type, id, ...fields });
    return id;
  }

  async request(type, fields = {}) {
    const id = await this.command(type, fields);
    return this.response(id);
  }

  async endInput(records = []) {
    const bytes = Buffer.from(records.map(record => JSON.stringify(record) + '\n').join(''));
    appendFileSync(join(this.directory, 'stdin.raw'), bytes);
    this.note('stdin-eof', { bytes: bytes.length, sha256: sha256(bytes) });
    await new Promise((resolve, reject) => this.child.stdin.end(bytes, error => error ? reject(error) : resolve()));
  }

  signal(signal) {
    assert(['SIGHUP', 'SIGTERM'].includes(signal), 'Product tests only send HUP or TERM');
    assert(this.pid && !this.exited && !this.closed, 'Refusing signal to a process no longer owned/live');
    const delivered = this.child.kill(signal);
    this.note('signal', { pid: this.pid, pgid: this.pgid, signal, delivered });
    assert(delivered, `Could not signal owned PID ${this.pid}`);
  }

  killOwnedGroup(reason) {
    if (!this.pid || this.closed) return;
    // detached:true makes this PGID our child's, never the agent or user's group.
    // Until close, descendants may retain its pipes even if the leader has exited.
    try {
      if (this.pgid) process.kill(-this.pgid, 'SIGKILL');
      else if (!this.exited) this.child.kill('SIGKILL');
      this.note('cleanup-signal', { pid: this.pid, pgid: this.pgid, signal: 'SIGKILL', reason });
    } catch (error) {
      if (error.code !== 'ESRCH') throw error;
    }
  }

  pauseAfterBytes(additionalBytes = 64 * 1024) {
    assert(!this.paused);
    this.#pauseAt = this.bytes + additionalBytes;
    this.note('arm-stdout-pause', { atByte: this.#pauseAt });
  }

  waitForPause() { return this.waitUntil(() => this.paused, { label: 'stdout pause barrier' }); }

  resumeOutput() {
    if (!this.paused) return;
    this.paused = false;
    this.note('stdout-resumed', { bytes: this.bytes });
    this.child.stdout.resume();
  }

  async waitForClose() { return this.completed; }

  async cleanup() {
    this.resumeOutput();
    if (!this.closed) this.killOwnedGroup('finally');
    return this.completed;
  }
}
