import { describe, it, expect } from 'vitest';
import { paceVerdict } from './pace';

describe('paceVerdict', () => {
  it('is "too early" when barely any time elapsed', () => {
    expect(paceVerdict(0.5, 0.02)).toEqual({ ratio: null, text: 'too early', tone: 'ink' });
  });
  it('flags >=1.5x as bad', () => {
    const v = paceVerdict(0.62, 0.31);
    expect(v.tone).toBe('bad');
    expect(v.ratio).toBeCloseTo(2.0, 5);
    expect(v.text).toContain('faster than linear');
  });
  it('flags 1.12..1.5x as warn', () => {
    expect(paceVerdict(0.30, 0.25).tone).toBe('warn');
  });
  it('calls <=0.85x under pace', () => {
    const v = paceVerdict(0.25, 0.50);
    expect(v.tone).toBe('ok');
    expect(v.text).toBe('under linear pace');
  });
  it('calls the middle band on pace', () => {
    expect(paceVerdict(0.50, 0.50).text).toBe('on pace');
  });
  it('0.04 elapsed is the boundary - not too early', () => {
    const v = paceVerdict(0.04, 0.04);
    expect(v.ratio).not.toBeNull();
    expect(v.text).not.toBe('too early');
  });
});
