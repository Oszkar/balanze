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
  it('is hidden when the provider is disabled', () => {
    expect(openaiColumnState({ enabled: false, hasData: false, error: null }).kind).toBe('hidden');
  });
  it('is data when enabled and data is present', () => {
    expect(openaiColumnState({ enabled: true, hasData: true, error: null }).kind).toBe('data');
  });
  it('is error when enabled and an error is set', () => {
    expect(openaiColumnState({ enabled: true, hasData: false, error: 'x' })).toEqual({ kind: 'error', message: 'x' });
  });
  it('is connect when enabled, configured-but-empty (no data, no error)', () => {
    expect(openaiColumnState({ enabled: true, hasData: false, error: null }).kind).toBe('connect');
  });
});
