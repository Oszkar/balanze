import { describe, it, expect } from 'vitest';
import { microUsdToCents, microUsdToDollars, relativeReset, formatBurn } from './format';
import policy from '../../../tests/fixtures/presentation-policy.json';

describe('format', () => {
  it('micro-usd -> dollars', () => expect(microUsdToDollars(12_740_000)).toBe('$12.74'));
  it('micro-usd -> dollars groups thousands', () => expect(microUsdToDollars(1_234_560_000)).toBe('$1,234.56'));
  it('micro-usd -> dollars zero', () => expect(microUsdToDollars(0)).toBe('$0.00'));

  it('matches the shared cent-rounding vectors', () => {
    // The same vectors drive the Rust `money` crate. If this fails there, the
    // two languages have drifted apart again.
    expect(policy.money.length).toBeGreaterThan(0);
    for (const c of policy.money) {
      expect(microUsdToCents(c.microUsd), `${c.microUsd} micro-USD`).toBe(c.cents);
    }
  });

  it('rounds half a cent away from zero, matching Rust', () => {
    // The regression: Intl rounded the shortest decimal of 1.005 up to $1.01
    // while Rust's `{:.2}` rounded the exact double 1.00499999... down to
    // $1.00. Rounding the integer first removes the disagreement.
    expect(microUsdToDollars(1_005_000)).toBe('$1.01');
    expect(microUsdToDollars(1_004_999)).toBe('$1.00');
    expect(microUsdToDollars(1_015_000)).toBe('$1.02');
  });

  it('never renders an amount that rounds to zero as negative zero', () => {
    expect(microUsdToDollars(-1)).toBe('$0.00');
    expect(microUsdToDollars(-4_999)).toBe('$0.00');
  });
  it('burn formats', () => {
    expect(formatBurn(null)).toBe('-');
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
