import { describe, it, expect } from 'vitest';
import { microUsdToDollars, relativeReset, formatBurn } from './format';

describe('format', () => {
  it('micro-usd → dollars', () => expect(microUsdToDollars(12_740_000)).toBe('$12.74'));
  it('burn formats', () => {
    expect(formatBurn(null)).toBe('—');
    expect(formatBurn(3200)).toBe('~3.2k/min');
    expect(formatBurn(840)).toBe('~840/min');
    expect(formatBurn(1000)).toBe('~1.0k/min');
  });
  it('relative reset', () => {
    const now = new Date('2026-06-03T12:00:00Z');
    expect(relativeReset('2026-06-03T14:41:00Z', now)).toBe('2h 41m');
    expect(relativeReset('2026-06-06T16:00:00Z', now)).toBe('3d 4h');
    expect(relativeReset('2026-06-03T11:00:00Z', now)).toBe('(passed)');
  });
});
