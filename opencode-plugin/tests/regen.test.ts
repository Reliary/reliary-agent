// Simple unit tests for regen helpers. We don't mock child_process and fs
// here because the harness is fragile; we just verify pure functions.

import { describe, it, expect } from 'vitest';
import { findProjectRoot, DEFAULT_FILE_PATTERN } from '../src/regen.js';

describe('DEFAULT_FILE_PATTERN regex', () => {
  it('matches .rs files', () => {
    expect(DEFAULT_FILE_PATTERN.test('lib.rs')).toBe(true);
  });
  it('matches .py files', () => {
    expect(DEFAULT_FILE_PATTERN.test('script.py')).toBe(true);
  });
  it('matches .ts/.tsx files', () => {
    expect(DEFAULT_FILE_PATTERN.test('app.ts')).toBe(true);
    expect(DEFAULT_FILE_PATTERN.test('app.tsx')).toBe(true);
  });
  it('matches .go files', () => {
    expect(DEFAULT_FILE_PATTERN.test('main.go')).toBe(true);
  });
  it('matches .c/.cpp/.h/.hpp files', () => {
    expect(DEFAULT_FILE_PATTERN.test('foo.c')).toBe(true);
    expect(DEFAULT_FILE_PATTERN.test('foo.cpp')).toBe(true);
    expect(DEFAULT_FILE_PATTERN.test('foo.h')).toBe(true);
    expect(DEFAULT_FILE_PATTERN.test('foo.hpp')).toBe(true);
  });
  it('rejects non-source extensions', () => {
    expect(DEFAULT_FILE_PATTERN.test('photo.png')).toBe(false);
    expect(DEFAULT_FILE_PATTERN.test('data.json')).toBe(false);
    expect(DEFAULT_FILE_PATTERN.test('README.md')).toBe(false);
    expect(DEFAULT_FILE_PATTERN.test('config.yml')).toBe(false);
  });
  it('is case-insensitive', () => {
    expect(DEFAULT_FILE_PATTERN.test('Foo.RS')).toBe(true);
    expect(DEFAULT_FILE_PATTERN.test('Baz.PY')).toBe(true);
    expect(DEFAULT_FILE_PATTERN.test('Quux.TS')).toBe(true);
  });
});

describe('findProjectRoot', () => {
  it('returns null for empty input', () => {
    expect(findProjectRoot('')).toBeNull();
  });
  it('returns null for invalid paths', () => {
    expect(findProjectRoot('/nonexistent/path/file.rs')).toBeNull();
  });
});
