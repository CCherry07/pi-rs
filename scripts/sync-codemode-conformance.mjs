#!/usr/bin/env node
// Execute the current local legacy coding-agent codemode, with deterministic in-process
// tools only. No providers, credentials, MCP servers or production filesystem tools run.
// Rust consumes the fixture without Node/legacy installed.
// Run: node --import ./legacy/pi/packages/coding-agent/src/experimental/source-resolver.ts scripts/sync-codemode-conformance.mjs [--check]
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
// Configure the documented package-root override before loading the oracle, so
// error guidance uses a portable docs path without normalizing message content.
process.env.PI_PACKAGE_DIR = "/pi";
const { executeCodemode } = await import("../legacy/pi/packages/coding-agent/src/extensions/codemode/execute.ts");

const root = fileURLToPath(new URL("../", import.meta.url));
const target = new URL("../plugins/features/pi-plugin-codemode/tests/fixtures/legacy-conformance.json", import.meta.url);
// The revision in the fixture must describe the code that actually executed.
execFileSync("git", ["-C", `${root}/legacy/pi`, "diff", "--quiet", "HEAD", "--"]);
const png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";
const tool = (name, description, extra = {}) => ({
  name, description,
  parameters: { type: "object", properties: { value: { type: "string" } } },
  ...extra,
});
const definitions = [
  tool("echo", "Echo a supplied value."),
  tool("structured", "Return a structured value.", { outputSchema: { type: "object", properties: { value: {} }, required: ["value"] } }),
  tool("fail", "Return an ordinary tool failure."),
  tool("structured_fail", "Return a structured error.", { outputSchema: { type: "object", properties: { isError: { type: "boolean" } }, required: ["isError"] } }),
  tool("mcp__demo-name__lookup", "Look up a demo record.", {
    namespace: { name: "demo-name", description: "Demo lookup tools", instructions: "Only use test records." },
    guidelines: ["Do not invent record identifiers."],
  }),
  tool("alpha", "Alpha namespace lookup.", { namespace: { name: "mcp__dev-radius", aliases: ["dev-radius"] } }),
  tool("empty_meta", "Empty metadata lookup.", { namespace: { name: "empty-meta", description: "", instructions: "" } }),
  tool("foo-bar", "First colliding tool."),
  tool("foo_bar", "Second colliding tool."),
  tool("models.getModelsOfType", "An ordinary tool, not the models facade."),
  tool("deep_roundtrip", "Return a structured JSON value.", { parameters: { type: "object", properties: { value: {} } }, outputSchema: { type: "object", properties: { value: {} }, required: ["value"] } }),
];
const modelDefinitions = [
  { type: "classifier", provider: "fixture", id: "z-classifier", name: "Z Classifier", api: "fixture", baseUrl: "https://fixture.invalid", input: ["text", "image"], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 128000 },
  { type: "classifier", provider: "fixture", id: "a-classifier", name: "A Classifier", api: "fixture", baseUrl: "https://fixture.invalid", input: ["text"], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 128000 },
  { type: "image", provider: "fixture", id: "painter", name: "Painter", api: "fixture", baseUrl: "https://fixture.invalid", input: ["text", "image"], output: ["image"], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } },
];
const normalTools = definitions.slice(0, 5).map((tool) => tool.name);
const cases = [];
const add = (name, code, options = {}) => cases.push({ name, code, ...options });
add("await-return", "text(await Promise.resolve('one')); return {answer:42};");
add("text-console-layout", "text('one\\ntwo'); console.log('a'); console.warn('b',2); text('three\\n'); return 4;");
add("empty-output", "return undefined;");
add("empty-text-items", "text(''); text(''); console.log('');");
add("text-primitives", "text(undefined);text(null);text(NaN);text(Infinity);text(-Infinity);text(-0);text(12n);text(Symbol('s'));");
add("text-error-object", "text(new Error('test error'));");
add("text-circular-rejects", "const v={};v.self=v;try{text(v)}catch(e){text({name:e.name,message:e.message})}");
add("text-nested-bigint-rejects", "try{text({n:1n})}catch(e){text({name:e.name,message:e.message})}");
add("return-nan", "return NaN;");
add("return-function", "return ()=>1;");
add("return-tojson", "return {toJSON(){return 'converted'}};");
add("return-deep-json", "let x=0;for(let i=0;i<200;i++)x={x};return x;");
add("return-bigint-fails", "text('before');return 1n;", { normalizeStack: true });
add("partial-output-error", "text('before');throw new Error('expected failure');", { normalizeStack: true });
add("non-error-throw", "text('before');throw {problem:'expected'};");
add("stalled-promise", "await new Promise(()=>{});");
add("exit-keeps-prefix", "text('before');exit();text('after');");
add("caught-exit-still-ends", "text('before');try{exit()}catch{}text('after');return 'after';");
add("exit-prevents-later-tool-dispatch", "try{exit()}catch{}tools.echo({value:'must not run'});", { tools: ["echo"] });
add("output-limit-prevents-later-tool-dispatch", "try{text('x'.repeat(16*1024*1024+1))}catch{}tools.echo({value:'must not run'});", { tools: ["echo"], normalizeStack: true });
add("globals-isolated", "text([typeof process,typeof require,typeof fetch,typeof setTimeout,typeof WebAssembly]);text(Object.isFrozen(Array.prototype));");
add("builtins-frozen", "Array.prototype.push=()=>0;const a=[];a.push(1);return a;");
add("builtins-frozen-strict", "'use strict';try{Array.prototype.push=()=>0;text('changed')}catch(e){text(e.name)}");
add("wrapper-lexical-scope", "return [this===globalThis,typeof arguments];");
add("wrapper-parameter-redeclaration", "const tools={};return 1;", { normalizeStack: true });
add("wrapper-sloppy-globals", "globalLeak=3;return globalThis.globalLeak;");
add("source-extra-input-properties", "return 1;", { extraInput: { ignored: true } });
add("error-subclass-overrides", "class MyError extends Error{constructor(){super('test');this.name='MyError'}}; const e=new MyError();text([e.name,e.message]);");
add("store-copy-delete", "store('__proto__',{ok:true});const v=load('__proto__');v.ok=false;text(load('__proto__'));store('__proto__',undefined);text(load('__proto__'));store('nil',null);text(load('nil'));");
add("store-deep-json", "let x=0;for(let i=0;i<200;i++)x={x};store('deep',x);return load('deep');");
add("store-max-supported-depth", "let x=0;for(let i=0;i<512;i++)x={x};store('deep',x);return load('deep');");
add("store-key-validation", "for(const fn of [store,load])try{fn(42,1)}catch(e){text({name:e.name,message:e.message})}");
add("store-nonjson-validation", "try{store('x',()=>1)}catch(e){text({name:e.name,message:e.message})}");
add("store-value-limit", "try{store('x','a'.repeat(262143))}catch(e){text({name:e.name,message:e.message})}");
add("store-unicode-limit", "store('x','汉'.repeat(262142));text(load('x').length);");
add("utf16-output-budget", "// @options: {\"max_output_tokens\":2}\ntext('你好世界');");
add("head-tail-spill", "// @options: {\"max_output_tokens\":2}\ntext('0123456789ABCDEFGHIJ');");
add("astral-head-tail-spill", "// @options: {\"max_output_tokens\":2}\ntext('😀😀😀😀😀😀');");
add("zero-output-budget", "// @options: {\"max_output_tokens\":0}\ntext('something');");
add("more-than-ten-thousand-items", "// @options: {\"max_output_tokens\":1}\nfor(let i=0;i<10001;i++)text('');");
add("source-above-one-mib", "return 7;", { prefixSpaces: 1048577 });
add("safe-integer-number-options", "// @options: {\"max_output_tokens\":2.0,\"timeout_ms\":60000.0}\nreturn 1;");
add("image-output-order", `text('before');image('data:image/png;base64,${png}');image({type:'image',data:'${png}',mimeType:'image/png'});text('after');`);
add("image-zero-text-budget", `// @options: {"max_output_tokens":0}\nimage('data:image/png;base64,${png}');`);
add("image-sniffs-mime", `image({type:'image',data:'${png}',mimeType:'image/jpeg'});`);
add("image-embedded-data-url", `image({type:'image',data:'data:image/png;base64,${png}',mimeType:'invalid'});`);
add("image-wrapped-base64", `image('DATA:image/jpeg;BASE64,${png.slice(0, 20)}\\n${png.slice(20)}');`);
add("image-invalid-signature", "try{image('data:image/png;base64,aGVsbG8=')}catch(e){text({name:e.name,message:e.message})}");
add("image-remote-url", "try{image('https://example.invalid/picture.png')}catch(e){text({name:e.name,message:e.message})}");
add("image-invalid-inputs", "for(const v of [null,[],{}, {type:'text',text:'hi'}, {type:'image',data:''}])try{image(v)}catch(e){text({name:e.name,message:e.message})}");
add("nested-projection", "text(await tools.echo({value:'hello'}));text(await tools.structured({value:'ok'}));text(await tools.structured_fail({}));", { tools: normalTools });
add("argument-and-result-deep-json", "let x=0;for(let i=0;i<200;i++)x={x};return (await tools.deep_roundtrip({value:x})).value;", { tools: ["deep_roundtrip"] });
add("argument-max-supported-depth", "let x=0;for(let i=0;i<511;i++)x={x};return (await tools.deep_roundtrip({value:x})).value;", { tools: ["deep_roundtrip"] });
add("nested-error-rejection", "try{await tools.fail({})}catch(e){text({name:e.name,message:e.message})}", { tools: normalTools });
add("nested-error-summary", "await tools.echo({value:'before'});await tools.fail({});", { tools: normalTools, normalizeStack: true });
add("more-than-128-calls", "let sum=0;for(let i=0;i<130;i++)sum+=(await tools.echo({value:'x'})).length;return sum;", { tools: normalTools });
add("argument-above-one-mib", "return (await tools.echo({value:'x'.repeat(1048577)})).length;", { tools: normalTools });
add("result-above-eight-mib", "return (await tools.echo({value:'x'.repeat(8388609)})).length;", { tools: normalTools });
add("all-tools-identifiers", "return ALL_TOOLS.map(t=>t.name);", { tools: normalTools });
add("tool-description", "return await describeTool('mcp__demo_name__lookup');", { tools: normalTools });
add("namespace-description", "return await describeNamespace('demo_name');", { tools: normalTools });
add("namespace-unknown", "text(await describeNamespace('unknown'));text(await describeTool('unknown'));", { tools: normalTools });
add("empty-namespace-filter", "return (await searchTools('echo',{namespace:''})).map(t=>t.name);", { tools: normalTools });
add("missing-tool-hint", "try{void tools.ech}catch(e){text({name:e.name,message:e.message})}", { tools: normalTools });
add("missing-tool-membership", "return ['missing' in tools, 'echo' in tools, tools.then,tools.toJSON];", { tools: normalTools });
add("identifier-first-wins", "text(ALL_TOOLS.map(t=>t.name));text(await tools.foo_bar({}));text(await tools['foo-bar']({}));", { tools: ["foo-bar", "foo_bar"] });
add("namespace-query-normalization", "return await describeNamespace('demo name');", { tools: normalTools });
add("namespace-alias", "return await describeNamespace('dev_radius');", { tools: ["alpha"] });
add("namespace-alias-search", "return (await searchTools('lookup',{namespace:'dev radius'})).map(t=>t.name);", { tools: ["alpha"] });
add("namespace-absent-metadata", "return await describeNamespace('mcp__dev-radius');", { tools: ["alpha"] });
add("namespace-empty-metadata", "return await describeNamespace('empty-meta');", { tools: ["empty_meta"] });
add("discovery-large-integral-limit", "return (await searchTools('echo',{limit:1e20})).map(t=>t.name);", { tools: normalTools });
add("discovery-validation", "for(const args of [[42],['echo',{limit:0}],['echo',{limit:1.5}],['echo',{namespace:42}]])try{await searchTools(...args)}catch(e){text({name:e.name,message:e.message})}", { tools: normalTools });
add("source-empty", "");
add("source-misplaced-options", "text(1);\n// @options: {}");
add("source-options-without-code", "// @options: {}");
add("source-unknown-option", "// @options: {\"other\":1}\nreturn 1;");
add("source-timeout-zero", "// @options: {\"timeout_ms\":0}\nreturn 1;");
add("source-timeout-fraction", "// @options: {\"timeout_ms\":1.5}\nreturn 1;");
add("source-output-negative", "// @options: {\"max_output_tokens\":-1}\nreturn 1;");
add("source-output-unsafe-integer", "// @options: {\"max_output_tokens\":1e20}\nreturn 1;");

add("tool-model-namespace-is-not-global", "return await tools.models_getModelsOfType({value:'ordinary tool'});", { tools: ["models.getModelsOfType"] });
add("models-not-enabled", "return typeof models;");
const addModel = (name, code) => add(name, code, { models: true });
addModel("models-catalog-order", "return (await models.getModelsOfType('classifier')).map(m=>m.id);");
addModel("models-available-order", "return (await models.getAvailableOfType('classifier',null)).map(m=>m.id);");
addModel("models-catalog-provider", "return [await models.getModelsOfType('classifier','absent'),(await models.getModelsOfType('classifier','fixture')).map(m=>m.id)];");
addModel("models-info", "return await models.getModelOfType('image','fixture','painter');");
addModel("models-not-found", "return [await models.getModelOfType('image','fixture','absent')];");
addModel("models-catalog-validation", "for(const call of [()=>models.getModelsOfType(),()=>models.getAvailableOfType('unknown'),()=>models.getModelsOfType('classifier',42),()=>models.getModelOfType('classifier'),()=>models.getModelOfType(42,3,4)])try{await call()}catch(e){text(e.message)}");
addModel("models-ref-validation", "for(const m of [undefined,null,[],{},'name',{provider:'fixture',id:'absent'},{provider:'fixture',id:'painter'}])try{await models.classify(m,{})}catch(e){text(e.message)}");
addModel("models-classifier-context-validation", "const m={provider:'fixture',id:'z-classifier'};for(const c of [undefined,[],{}, {state:[]},{state:{},images:null},{state:{},images:[{}]},{state:{},questions:{}},{state:{},questions:{q:{instructions:'',type:'other'}}},{state:{},questions:{q:{instructions:'',type:'choice',criteria:{}}}},{state:{},questions:{q:{instructions:'',type:'score',criteria:[]}}},{state:{},questions:{q:{instructions:'',type:'bool',criteria:{}}}}])try{await models.classify(m,c)}catch(e){text(e.message)}");
addModel("models-images-context-validation", "const m={provider:'fixture',id:'painter'};for(const c of [undefined,[],{},{input:[]},{input:[null]},{input:[{type:'text',text:42}]},{input:[{type:'image',data:'x'}]}])try{await models.generateImages(m,c)}catch(e){text(e.message)}");
addModel("models-classifier-order-and-identity", "const r=await models.classify({provider:'fixture',id:'z-classifier',baseUrl:'https://untrusted.invalid',headers:{test:'ignored'}},{state:{},questions:{z:{type:'choice',instructions:'pick',criteria:{z:'last',a:'first'}},a:{type:'bool',instructions:'ok',criteria:{true:'yes',false:'no'}}}});return r;");
addModel("models-classifier-failure-is-value", "return await models.classify({provider:'fixture',id:'z-classifier'},{state:{fail:true},questions:{q:{type:'bool',instructions:'ok',criteria:{true:'yes',false:'no'}}}});");
addModel("models-images-success", `return await models.generateImages({provider:'fixture',id:'painter'},{input:[{type:'text',text:'fixture'},{type:'image',data:'${png}',mimeType:'image/png'}]});`);

addModel("models-images-text-signature", "return await models.generateImages({provider:'fixture',id:'painter'},{input:[{type:'text',text:'fixture',textSignature:'sig-v1'}]});");

function createModels() {
  const getModelsOfType = (type, provider) => modelDefinitions.filter((model) => model.type === type && (provider === undefined || model.provider === provider));
  return {
    getModelsOfType,
    getAvailableOfType: async (...args) => getModelsOfType(...args),
    getModelOfType: (type, provider, id) => getModelsOfType(type, provider).find((model) => model.id === id),
    classify: async (model, context) => {
      assert.equal(model.baseUrl, "https://fixture.invalid", "only canonical model identity is trusted");
      const answers = Object.fromEntries(Object.entries(context.questions).map(([id, question]) => {
        let answer;
        if (question.type === "choice") {
          const labels = Object.keys(question.criteria);
          answer = { type: "choice", choice: labels[0], probabilities: Object.fromEntries(labels.map((label, index) => [label, index === 0 ? 1 : 0])), confidence: 1 };
        } else if (question.type === "score") answer = { type: "score", score: 1, confidence: 1 };
        else answer = { type: "bool", probability: 1 };
        return [id, answer];
      }));
      return { api: model.api, provider: model.provider, model: model.id, answers, stopReason: context.state.fail ? "error" : "stop", ...(context.state.fail ? { errorMessage: "fixture model failure" } : {}), timestamp: 1 };
    },
    generateImages: async (model, context) => ({ api: model.api, provider: model.provider, model: model.id, output: context.input, stopReason: "stop", timestamp: 1 }),
  };
}

function normalizeText(value, scenario, files) {
  let text = value.replace(/Wall time \d+\.\d seconds/g, "Wall time <elapsed> seconds");
  for (const match of text.matchAll(/\[Image saved to (.+?) \((image\/[^,]+), ([^)]+)\)\]/g)) {
    const path = match[1];
    const data = readFileSync(path).toString("base64");
    const key = `image-${[...files.keys()].filter((key) => key.startsWith("image-")).length}`;
    if (![...files.values()].some((file) => file.path === path)) files.set(key, { path, data });
  }
  text = text.replace(/\[Image saved to (.+?) \((image\/[^,]+), ([^)]+)\)\]/g, (_all, path, mime, size) => {
    const id = [...files.entries()].find(([, file]) => file.path === path)[0];
    return `[Image saved to <${id}> (${mime}, ${size})]`;
  });
  if (scenario.normalizeStack) {
    // Engine-specific stack frames are not portable between QuickJS WASM and native
    // QuickJS. Preserve the error name/message and tool-side-effect summary verbatim.
    text = text.split("\n").filter((line) => !/^\s+at /.test(line)).join("\n");
  }
  return text;
}

async function evaluate(scenario) {
  const chosen = (scenario.tools ?? []).map((name) => definitions.find((tool) => tool.name === name));
  let count = 0;
  const ctx = {
    tools: chosen,
    modelRegistry: createModels(),
    sessionManager: { getBranch: () => [] },
    executeTool: async (name, args) => {
      const id = `oracle/${++count}`;
      const error = name === "fail" || name === "structured_fail";
      let result;
      if (name === "structured" || name === "deep_roundtrip") result = { content: [{ type: "text", text: "structured fallback" }], structuredContent: { value: args.value ?? null } };
      else if (name === "structured_fail") result = { content: [{ type: "text", text: "structured failure" }], structuredContent: { isError: true } };
      else if (name === "fail") result = { content: [{ type: "text", text: "tool rejected" }] };
      else result = { content: [{ type: "text", text: args.value ?? name }] };
      return { toolCall: { id, name, arguments: args }, result, isError: error };
    },
  };
  let result;
  try {
    result = await executeCodemode("oracle", { code: " ".repeat(scenario.prefixSpaces ?? 0) + scenario.code, ...scenario.extraInput }, undefined, undefined, ctx, {
      models: scenario.models === true,
      getToolNamespace: (name) => chosen.find((tool) => tool.name === name)?.namespace,
      getToolGuidelines: () => new Map(chosen.map((tool) => [tool.name, tool.guidelines ?? []])),
    });
  } catch (error) {
    assert.equal(error.name, "CodemodeSourceError", `${scenario.name}: unexpected host failure`);
    return { ...scenario, expected: { inputError: error.message } };
  }
  assert(!result.content.some((block) => block.type === "text" && block.text.includes("Script sandbox failed:")), `${scenario.name}: legacy sandbox could not execute`);
  if (scenario.models) {
    assert.notEqual(result.isError, true, `${scenario.name}: model fixture must complete (invalid inputs are caught)`);
    assert(!result.content.some((block) => block.type === "text" && block.text.includes("Cannot read properties of undefined")), `${scenario.name}: model mock was not connected`);
  }
  const files = new Map();
  try {
    const content = result.content.map((block) => block.type === "text" ? { type: "text", text: normalizeText(block.text, scenario, files) } : block);
    const expected = {
      isError: result.isError === true,
      content,
      calls: result.details.calls.map(({ name, args, status, error }) => ({ name, args, status, ...(error === undefined ? {} : { error }) })),
    };
    if (result.details.fullOutputPath) {
      const path = result.details.fullOutputPath;
      expected.fullOutput = normalizeText(readFileSync(path, "utf8"), scenario, files);
      for (const block of expected.content) if (block.type === "text") block.text = block.text.split(path).join("<full-output>");
      files.set("full-output", { path });
    }
    expected.savedImages = [...files.entries()].filter(([key]) => key.startsWith("image-")).map(([, file]) => file.data);
    return { ...scenario, expected };
  } finally {
    for (const file of files.values()) rmSync(file.path, { force: true });
  }
}

const fixture = {
  oracleRevision: execFileSync("git", ["-C", `${root}/legacy/pi`, "rev-parse", "HEAD"], { encoding: "utf8" }).trim(),
  tools: definitions,
  models: modelDefinitions,
  cases: [],
};
for (const scenario of cases) fixture.cases.push(await evaluate(scenario));
const output = `${JSON.stringify(fixture, null, 2)}\n`;
if (process.argv.includes("--check")) assert.equal(readFileSync(target, "utf8"), output, "Codemode oracle fixture is stale");
else {
  mkdirSync(new URL("./", target), { recursive: true });
  writeFileSync(target, output);
}
console.log(`Verified ${fixture.cases.length} codemode cases against legacy ${fixture.oracleRevision}`);
