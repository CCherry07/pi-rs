import { AsyncLocalStorage } from "node:async_hooks";
import { homedir } from "node:os";
import { join } from "node:path";

const shared = globalThis as typeof globalThis & {
  __piRsAgentDirStorage?: AsyncLocalStorage<string>;
};
const currentAgentDir = shared.__piRsAgentDirStorage ??= new AsyncLocalStorage<string>();

export const getAgentDir = () => currentAgentDir.getStore() ?? process.env.PI_AGENT_DIR ?? join(homedir(), ".pi", "agent");
export const withAgentDir = <T>(agentDir: string, callback: () => T): T => currentAgentDir.run(agentDir, callback);
