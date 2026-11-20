import { isDeepStrictEqual } from 'node:util';

export const NORMALIZATION = [
  'Generated session IDs in get_state, generated UI request IDs, and journal entry/references: one-to-one first-seen aliases.',
  'Message/journal/turn_start timestamps and message/tool durations: preserve field presence and JSON type; map finite numeric clock values to zero and parseable timestamp strings to <timestamp>.',
  'No other keys or records removed. No event/response sorting. No usage synthesis. Unknown/custom entry payloads, tool arguments, details and structuredContent are opaque.',
  'Sandbox and provider URL are identical across targets within a case; no path rewriting is needed.',
];

export function normalize(records) {
  const aliases = { entry: new Map(), session: new Map(), ui: new Map() };
  const mappings = [];
  function change(path, from, to, reason) {
    if (!isDeepStrictEqual(from, to)) mappings.push({ path, from, to, reason });
    return to;
  }
  function id(kind, value, path) {
    if (value === null || typeof value !== 'string') return value;
    const map = aliases[kind];
    if (!map.has(value)) map.set(value, `<${kind}:${map.size + 1}>`);
    return change(path, value, map.get(value), `${kind} identity`);
  }
  function visit(value, path, record) {
    if (Array.isArray(value)) return value.map((item, index) => visit(item, `${path}/${index}`, record));
    if (!value || typeof value !== 'object') return value;
    const entry = typeof value.type === 'string' && Object.hasOwn(value, 'id') && Object.hasOwn(value, 'parentId');
    const message = typeof value.role === 'string';
    const responseData = path.endsWith('/data') && path.split('/').length === 3 && record.type === 'response';
    const result = {};
    for (const [key, item] of Object.entries(value)) {
      const location = `${path}/${key}`;
      if (['details', 'structuredContent', 'arguments', 'args'].includes(key) || (entry && key === 'data')) {
        result[key] = item;
      } else if ((key === 'timestamp' && (entry || message || value.type === 'turn_start'))
        || (key === 'durationMs' && (message || value.type === 'tool_execution_end'))) {
        // Invalid/type-changed values remain visible as differences, never coerced.
        if (typeof item === 'number' && Number.isFinite(item)) result[key] = change(location, item, 0, 'clock value');
        else if (key === 'timestamp' && typeof item === 'string' && Number.isFinite(Date.parse(item))) result[key] = change(location, item, '<timestamp>', 'clock string');
        else result[key] = item;
      } else if (responseData && record.command === 'get_state' && key === 'sessionId') {
        result[key] = id('session', item, location);
      } else if (key === 'id' && value.type === 'extension_ui_request') {
        result[key] = id('ui', item, location);
      } else if ((entry && ['id', 'parentId', 'firstKeptEntryId'].includes(key))
        || (key === 'messageEntryId' && value.type === 'turn_end')
        || (responseData && ['get_entries', 'get_tree'].includes(record.command) && key === 'leafId')
        || (record.command === 'get_fork_messages' && key === 'entryId')) {
        result[key] = id('entry', item, location);
      } else if (Array.isArray(item) && ((entry && key === 'retainedEntryIds')
        || (value.type === 'turn_end' && key === 'toolResultEntryIds'))) {
        result[key] = item.map((value, index) => id('entry', value, `${location}/${index}`));
      } else result[key] = visit(item, location, record);
    }
    return result;
  }
  return { records: records.map((record, index) => visit(record, `/${index}`, record)), mappings, rules: NORMALIZATION };
}

const preview = value => {
  const encoded = JSON.stringify(value);
  return encoded === undefined ? { missing: true } : { json: encoded.slice(0, 400), characters: encoded.length };
};

function firstDifference(left, right, path = '') {
  if (isDeepStrictEqual(left, right)) return null;
  if (left && right && typeof left === 'object' && typeof right === 'object' && Array.isArray(left) === Array.isArray(right)) {
    if (Array.isArray(left) && left.length !== right.length) return { path: `${path}/length`, left: left.length, right: right.length };
    for (const key of new Set([...Object.keys(left), ...Object.keys(right)])) {
      if (Object.hasOwn(left, key) !== Object.hasOwn(right, key)) return { path: `${path}/${key}`, left: preview(left[key]), right: preview(right[key]) };
      const different = firstDifference(left[key], right[key], `${path}/${key}`);
      if (different) return different;
    }
  }
  return { path, left: preview(left), right: preview(right) };
}

export function compareStreams(left, right) {
  const oracle = normalize(left);
  const candidate = normalize(right);
  const equal = isDeepStrictEqual(oracle.records, candidate.records);
  return {
    equal, firstDifference: firstDifference(oracle.records, candidate.records),
    recordCounts: { oracle: left.length, candidate: right.length }, oracle, candidate,
  };
}
