export type Tone = 'ok' | 'warn' | 'bad' | 'ink';
export interface PaceVerdict { ratio: number | null; text: string; tone: Tone; }

export function paceVerdict(usedFraction: number, elapsedFraction: number): PaceVerdict {
  if (elapsedFraction < 0.04) return { ratio: null, text: 'too early', tone: 'ink' };
  const r = usedFraction / elapsedFraction;
  if (r >= 1.5) return { ratio: r, text: `${r.toFixed(1)}× faster than linear`, tone: 'bad' };
  if (r >= 1.12) return { ratio: r, text: `${r.toFixed(1)}× faster than linear`, tone: 'warn' };
  if (r <= 0.85) return { ratio: r, text: 'under linear pace', tone: 'ok' };
  return { ratio: r, text: 'on pace', tone: 'ok' };
}
