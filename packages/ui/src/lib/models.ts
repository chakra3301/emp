/** Copilot is disabled across EMP, including replies from an older daemon. */
export function isCopilotModel(id: string, group?: string | null): boolean {
  const copilot = (provider: string) => provider.toLowerCase().split(/[^a-z0-9]+/).includes("copilot");
  return copilot(id.split(/[/:]/, 1)[0] ?? id) || (!!group && copilot(group));
}
