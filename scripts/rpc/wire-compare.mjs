import assert from 'node:assert/strict';

const entryKinds = new Set(['message', 'model_change', 'thinking_level_change', 'compaction', 'branch_summary', 'custom', 'custom_message', 'label', 'session_info', 'context_edit']);

// Only generated identities and clock values in known protocol positions change.
// Extension data and field presence/types, including explicit null, remain intact.
export function normalizeRpcRecords(records) {
  const ids = { entry: new Map(), session: new Map(), ui: new Map() };
  const identity = (kind, value) => {
    if (value === null) return null;
    assert.equal(typeof value, 'string');
    const map = ids[kind];
    if (!map.has(value)) map.set(value, `<${kind}-${map.size}>`);
    return map.get(value);
  };
  function visit(value, path, record) {
    if (Array.isArray(value)) return value.map((item, index) => visit(item, [...path, index], record));
    if (value === null || typeof value !== 'object') return value;
    const entry = entryKinds.has(value.type) && Object.hasOwn(value, 'parentId') && Object.hasOwn(value, 'id');
    const message = typeof value.role === 'string';
    const responseData = path.length === 1 && path[0] === 'data' && record.type === 'response';
    const result = Object.create(null);
    for (const [key, field] of Object.entries(value)) {
      if (['details', 'structuredContent', 'arguments', 'args'].includes(key) || (entry && value.type === 'custom' && key === 'data')) result[key] = field;
      else if ((key === 'timestamp' && (entry || message)) || (key === 'durationMs' && (value.type === 'tool_execution_end' || ['toolResult', 'assistant'].includes(value.role)))) {
        if (typeof field === 'number') { assert(Number.isFinite(field)); result[key] = 0; }
        else { assert.equal(key, 'timestamp'); assert.equal(typeof field, 'string'); assert(Number.isFinite(Date.parse(field))); result[key] = '<timestamp>'; }
      } else if (key === 'sessionId' && responseData && ['get_state', 'get_session_stats'].includes(record.command)) result[key] = identity('session', field);
      else if ((entry && ['id', 'parentId', 'firstKeptEntryId'].includes(key)) || (key === 'messageEntryId' && value.type === 'turn_end') || (key === 'leafId' && responseData && record.command === 'get_entries')) result[key] = identity('entry', field);
      else if ((key === 'toolResultEntryIds' && value.type === 'turn_end') || (key === 'retainedEntryIds' && entry)) { assert(Array.isArray(field)); result[key] = field.map(id => identity('entry', id)); }
      else if (key === 'id' && value.type === 'extension_ui_request') result[key] = identity('ui', field);
      else result[key] = visit(field, [...path, key], record);
    }
    return result;
  }
  return { records: records.map(record => visit(record, [], record)), ids: Object.fromEntries(Object.entries(ids).map(([kind, values]) => [kind, Object.fromEntries(values)])) };
}
