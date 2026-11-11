// Compaction limits: when a chat's context passes this many tokens, EMP has the agent compact it.
// Range mirrors empd (`rpc::COMPACT_MIN`/`COMPACT_MAX`).

export const COMPACT_MIN = 50_000;
export const COMPACT_MAX = 1_000_000;
export const COMPACT_PRESETS = [100_000, 150_000, 200_000, 300_000] as const;

/** 150000 → "150k", 1000000 → "1M". */
export function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, "")}M`;
  return `${Math.round(n / 1000)}k`;
}

/** "150", "150k", "150000", "1m" → tokens; null when it isn't a valid limit. */
export function parseTokens(text: string): number | null {
  const m = /^\s*(\d+(?:\.\d+)?)\s*([km]?)\s*$/i.exec(text);
  if (!m) return null;
  const n = Number(m[1]);
  const unit = m[2]!.toLowerCase();
  // A bare number under 10,000 means thousands ("150" = 150k).
  const tokens = Math.round(unit === "m" ? n * 1_000_000 : unit === "k" || n < 10_000 ? n * 1000 : n);
  return tokens >= COMPACT_MIN && tokens <= COMPACT_MAX ? tokens : null;
}
