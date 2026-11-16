(function (request, output, complete, query, toolsJson, storeJson) {
  "use strict";
  // Host capabilities live only in this closure. User source is compiled by a
  // separate AsyncFunction, never evaluated in the closure's lexical scope.
  const parse = JSON.parse;
  const stringify = JSON.stringify;
  const define = Object.defineProperty;
  const ownKeys = Object.keys;
  const create = Object.create;
  const freeze = Object.freeze;
  const PromiseCtor = Promise;
  const ErrorCtor = Error;
  const StringCtor = String;
  const AsyncFunctionCtor = Object.getPrototypeOf(async function () {}).constructor;
  const then = Function.prototype.call.bind(Promise.prototype.then);
  const hasOwn = Function.prototype.call.bind(Object.prototype.hasOwnProperty);
  const pending = create(null);
  const stored = create(null);
  const changed = create(null);
  const EXIT = freeze({});
  let nextId = 1;
  let finished = false;

  function format(value) {
    if (typeof value === "string") return value;
    if (value === undefined) return "undefined";
    if (typeof value === "number") return StringCtor(value);
    if (value instanceof ErrorCtor) return errorText(value);
    try { return stringify(value) ?? StringCtor(value); }
    catch (_) { return StringCtor(value); }
  }
  function errorText(error) {
    try {
      if (error && typeof error.message === "string") {
        return (error.name ? StringCtor(error.name) + ": " : "") + error.message;
      }
      return format(error);
    } catch (_) { return "The script threw a value that cannot be described"; }
  }
  function text(value) {
    if (!finished) output("text", format(value), "");
  }
  function writes() {
    const set = create(null);
    const deleted = [];
    for (const key of ownKeys(changed)) {
      if (hasOwn(stored, key)) define(set, key, { value: parse(stored[key]), enumerable: true });
      else define(deleted, deleted.length, { value: key, enumerable: true });
    }
    return stringify({ set, delete: deleted });
  }
  function done(ok, error) {
    if (finished) return;
    const delta = ok ? writes() : "{}";
    complete(ok, error || "", delta);
    finished = true;
  }
  function exit() {
    done(true, "");
    throw EXIT;
  }
  function call(kind, name, args) {
    return new PromiseCtor((resolve, reject) => {
      if (finished) { reject(new ErrorCtor("Script has completed")); return; }
      try {
        const json = stringify(args === undefined ? {} : args);
        if (json === undefined) throw new ErrorCtor("Tool arguments must be JSON-serializable");
        const id = nextId++;
        define(pending, id, { value: { resolve, reject }, configurable: true });
        request(id, kind, name, json);
      } catch (error) { reject(error); }
    });
  }

  const tools = create(null);
  const all = parse(toolsJson);
  // Exact names are reserved before aliases, so normalization never shadows a tool.
  for (const entry of all) {
    define(tools, entry.name, { value: (args) => call("tool", entry.name, args), enumerable: true });
  }
  for (const entry of all) {
    if (!hasOwn(tools, entry.jsName)) {
      define(tools, entry.jsName, { value: tools[entry.name], enumerable: true });
    }
  }
  const allTools = [];
  for (const entry of all) {
    define(allTools, allTools.length, { value: freeze({ name: entry.jsName, description: entry.description }), enumerable: true });
  }
  const globals = { tools: freeze(tools), ALL_TOOLS: freeze(allTools), text, exit };
  const models = create(null);
  for (const name of ["getModelsOfType", "getAvailableOfType", "getModelOfType", "classify", "generateImages"]) {
    define(models, name, { value: async (...args) => {
      const result = await call("model", name, args);
      return result.found ? result.value : undefined;
    }, enumerable: true });
  }
  globals.models = freeze(models);
  for (const name of ["searchTools", "describeTool", "describeNamespace"]) {
    globals[name] = async (...args) => {
      const response = parse(query(name, stringify(args)));
      return response.found ? response.value : undefined;
    };
  }

  const initial = parse(storeJson);
  let storeChars = 0;
  for (const key of ownKeys(initial)) {
    const json = stringify(initial[key]);
    define(stored, key, { value: json, writable: true, configurable: true, enumerable: true });
    storeChars += key.length + json.length;
  }
  globals.store = (key, value) => {
    if (typeof key !== "string") throw new ErrorCtor("store() key must be a string");
    const previous = hasOwn(stored, key) ? key.length + stored[key].length : 0;
    if (value === undefined) {
      delete stored[key];
      storeChars -= previous;
    } else {
      const json = stringify(value);
      if (json === undefined) throw new ErrorCtor("store() value must be JSON-serializable");
      if (json.length > 256 * 1024) throw new ErrorCtor("store() value exceeds 256 KiB");
      const next = storeChars - previous + key.length + json.length;
      if (next > 1024 * 1024) throw new ErrorCtor("store() total exceeds 1 MiB");
      define(stored, key, { value: json, writable: true, configurable: true, enumerable: true });
      storeChars = next;
    }
    define(changed, key, { value: true, configurable: true, enumerable: true });
  };
  globals.load = (key) => {
    if (typeof key !== "string") throw new ErrorCtor("load() key must be a string");
    return hasOwn(stored, key) ? parse(stored[key]) : undefined;
  };
  globals.image = (value) => {
    if (finished) return;
    let data, mime;
    if (value && typeof value === "object" && value.type === "image") {
      data = value.data; mime = value.mimeType;
    } else {
      const url = typeof value === "string" ? value : value && value.image_url;
      if (typeof url !== "string") throw new ErrorCtor("image() expects a base64 data URL or image block");
      const match = /^data:(image\/[A-Za-z0-9.+-]+);base64,([A-Za-z0-9+/]*={0,2})$/.exec(url);
      if (!match) throw new ErrorCtor("image() supports only base64 image data URLs");
      mime = match[1]; data = match[2];
    }
    if (typeof data !== "string" || typeof mime !== "string") throw new ErrorCtor("image() expects string data and mimeType");
    output("image", data, mime);
  };
  const console = create(null);
  for (const name of ["log", "info", "warn", "error", "debug"]) {
    console[name] = (...args) => {
      if (finished) return;
      let line = "";
      for (let index = 0; index < args.length; index++) line += (index ? " " : "") + format(args[index]);
      output("console", line, "");
    };
  }
  globals.console = freeze(console);
  for (const key of ownKeys(globals)) define(globalThis, key, { value: globals[key], enumerable: true });
  return {
    run(code) {
      try {
        const fn = new AsyncFunctionCtor("\"use strict\";\n" + code);
        then(fn(), (value) => {
          if (finished) return;
          try { if (value !== undefined) text(value); done(true, ""); }
          catch (error) { done(false, errorText(error)); }
        }, (error) => { if (error !== EXIT) done(false, errorText(error)); });
      } catch (error) { if (error !== EXIT) done(false, errorText(error)); }
    },
    settle(id, ok, json) {
      if (finished || !hasOwn(pending, id)) return;
      const handlers = pending[id];
      delete pending[id];
      try {
        if (ok) handlers.resolve(parse(json));
        else handlers.reject(new ErrorCtor(json));
      } catch (error) { handlers.reject(error); }
    }
  };
})
