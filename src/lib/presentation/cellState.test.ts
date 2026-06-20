import { describe, it, expect } from 'vitest';
import { anthropicQuotaState, openaiColumnState } from './cellState';

describe('anthropicQuotaState', () => {
  it('is data when a quota is present', () => {
    expect(anthropicQuotaState({ hasQuota: true, error: null }).kind).toBe('data');
  });
  it('is error when a quota error is set', () => {
    expect(anthropicQuotaState({ hasQuota: false, error: 'boom' })).toEqual({ kind: 'error', message: 'boom' });
  });
  it('is loading on cold start (no quota, no error)', () => {
    expect(anthropicQuotaState({ hasQuota: false, error: null }).kind).toBe('loading');
  });
});

describe('openaiColumnState', () => {
  it('is hidden when no data and billing not opted in', () => {
    expect(openaiColumnState({ billingEnabled: false, hasData: false, error: null }).kind).toBe('hidden');
  });
  it('is data when data is present even if billing is not opted in (Codex-only)', () => {
    expect(openaiColumnState({ billingEnabled: false, hasData: true, error: null }).kind).toBe('data');
  });
  it('is data when billing is opted in and data is present', () => {
    expect(openaiColumnState({ billingEnabled: true, hasData: true, error: null }).kind).toBe('data');
  });
  it('is error when billing opted in, no data, error set', () => {
    expect(openaiColumnState({ billingEnabled: true, hasData: false, error: 'x' })).toEqual({ kind: 'error', message: 'x' });
  });
  it('is connect when billing opted in, no data, no error', () => {
    expect(openaiColumnState({ billingEnabled: true, hasData: false, error: null }).kind).toBe('connect');
  });
});
