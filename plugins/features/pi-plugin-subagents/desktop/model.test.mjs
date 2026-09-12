import test from "node:test";
import assert from "node:assert/strict";
import { decodeSubagentsWidget, itemTasks, widgetTask, isControllable } from "./src/model.ts";

const item = (data, status = "completed") => ({id:"call",toolType:"mcpToolCall",title:"spawn_agent",detail:"",status,data});
const task = {agentId:"agent",agent:"reviewer",state:"interrupted",session:{sessionId:"child",ownerSessionId:"parent"},totalTokens:321};
const widget = {version:1,ownerSessionId:"parent",agents:{agent:task},liveAgentIds:["agent"]};
const decodedWidget = decodeSubagentsWidget(widget);

test("the async spawn receipt never fabricates successful child completion", () => {
  assert.equal(itemTasks(item({arguments:{agent:"reviewer",task:"review"},details:{agentId:"agent",state:"running",sessionId:"child"}}), "parent")[0].state, "running");
  assert.equal(itemTasks(item({arguments:{agent:"reviewer"}}), "parent")[0].state, undefined);
  assert.equal(itemTasks(item({}, "failed"), "parent")[0].state, "failed");
});

test("runtime widgets preserve terminal state and exact ownership", () => {
  assert.deepEqual(widgetTask(decodedWidget,"agent"), {...task,task:undefined,session:{...task.session,isolatedSessionId:undefined}});
  assert.equal(isControllable(decodedWidget,task,"parent"),true);
  assert.equal(isControllable(decodedWidget,task,"other"),false);
  assert.equal(isControllable(decodeSubagentsWidget({...widget,liveAgentIds:[]}),task,"parent"),false);
  assert.equal(isControllable(decodeSubagentsWidget({...widget,ownerSessionId:"other"}),task,"parent"),false);
  assert.throws(() => decodeSubagentsWidget({...widget,version:2}));
});

test("legacy raw receiver arrays preserve multiple conversations and token totals", () => {
  const tasks = itemTasks(item({newThreadId:"child",newAgentRole:"reviewer",prompt:"review",receiverThreadIds:["child","sibling"],
    receiverAgents:[{threadId:"sibling",agentNickname:"Scout"}],
    agentStatuses:[{threadId:"child",status:"completed",totalTokens:321},{threadId:"sibling",status:"failed"}],
  }), "parent");
  assert.equal(tasks.length,2);
  assert.equal(tasks[0].totalTokens,321);
  assert.equal(tasks[0].agent,"reviewer");
  assert.equal(tasks[1].agent,"Scout");
  assert.equal(tasks[1].state,"failed");
});

test("malformed or foreign widget data cannot authorize historical cards", () => {
  assert.equal(widgetTask(decodeSubagentsWidget({...widget,agents:{agent:{...task,agentId:"other"}}}),"agent"),undefined);
  assert.equal(isControllable(null,task,"parent"),false);
  assert.equal(itemTasks(item(null),"parent").length,1);
  assert.throws(() => decodeSubagentsWidget({...widget,agents:[task]}));
  assert.throws(() => decodeSubagentsWidget({...widget,liveAgentIds:[1]}));
});
