export default function botmuxTurnBoundary(pi: any) {
  pi.on("agent_settled", () => {
    pi.appendEntry("botmux-turn-settled", { source: "botmux-e2e" });
  });
}
