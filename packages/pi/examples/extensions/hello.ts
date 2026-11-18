// Load with: pi --extension ./hello.ts
export default function (pi: { registerCommand(name: string, options: unknown): void }) {
  pi.registerCommand("hello", {
    description: "Show a greeting",
    handler: async (_args: string, ctx: { ui: { notify(message: string, level: string): void } }) => {
      ctx.ui.notify("Hello from the extension", "info");
    },
  });
}
