import type { AgentPluginManifest, CommandManifest } from './extension-protocol.js'

export type UnplannedCommandManifest = Omit<CommandManifest, 'invocationName'>

export interface UnplannedAgentPluginManifest extends Omit<AgentPluginManifest, 'commands'> {
  commands: UnplannedCommandManifest[]
}

/** Applies Pi's JavaScript registration policy before Rust freezes a generation. */
export class RegistrationPlanner {
  readonly #toolOwners = new Map<string, string>()

  acceptsTool(pluginId: string, name: string): boolean {
    const owner = this.#toolOwners.get(name)
    if (owner !== undefined) return owner === pluginId
    this.#toolOwners.set(name, pluginId)
    return true
  }

  plan(plugins: readonly UnplannedAgentPluginManifest[]): AgentPluginManifest[] {
    const toolNames = new Set<string>()
    const commandCounts = new Map<string, number>()
    for (const plugin of plugins) {
      for (const command of plugin.commands) {
        commandCounts.set(command.name, (commandCounts.get(command.name) ?? 0) + 1)
      }
    }
    const commandOccurrences = new Map<string, number>()
    const invocationNames = new Set<string>()

    return plugins.map(plugin => ({
      ...plugin,
      tools: plugin.tools.filter(tool => {
        if (toolNames.has(tool.name)) return false
        toolNames.add(tool.name)
        return true
      }),
      commands: plugin.commands.map(command => {
        const occurrence = (commandOccurrences.get(command.name) ?? 0) + 1
        commandOccurrences.set(command.name, occurrence)
        let invocationName = (commandCounts.get(command.name) ?? 0) > 1
          ? `${command.name}:${occurrence}`
          : command.name
        if (invocationNames.has(invocationName)) {
          let suffix = occurrence
          do {
            suffix++
            invocationName = `${command.name}:${suffix}`
          } while (invocationNames.has(invocationName))
        }
        invocationNames.add(invocationName)
        return { ...command, invocationName }
      }),
    }))
  }
}
