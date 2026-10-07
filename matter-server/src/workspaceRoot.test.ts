import { afterEach, describe, expect, it } from 'vitest';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const findRoot = require('../vendor/find-yarn-workspace-root/index.cjs') as (path: string) => string | null;
const fixtures: string[] = [];
function workspace(patterns: string[] | { packages: string[] }, member = 'packages/client') {
  const root = mkdtempSync(join(tmpdir(), 'pond-workspace-'));
  fixtures.push(root);
  const child = join(root, member);
  mkdirSync(child, { recursive: true });
  writeFileSync(join(root, 'package.json'), JSON.stringify({ private: true, workspaces: patterns }));
  writeFileSync(join(child, 'package.json'), JSON.stringify({ name: 'client' }));
  return { root, child };
}
afterEach(() => fixtures.splice(0).forEach(root => rmSync(root, { recursive: true, force: true })));

describe('BLE installer workspace lookup', () => {
  it.each([
    [['packages/*'], true],
    [['packages/**'], true],
    [['{packages,apps}/*'], true],
    [['packages/@(client|server)'], true],
    [['packages/*', '!packages/client'], false],
    [['!packages/client', 'packages/*'], true],
    [['!packages/server'], true],
    [['!packages/client'], false],
    [['apps/*'], false],
    [[], false],
  ] as [string[], boolean][])('matches ordered patterns %j', (patterns, matches) => {
    const { root, child } = workspace(patterns);
    expect(findRoot(child)).toBe(matches ? root : null);
  });
  it('accepts the object form and the root itself', () => {
    const { root, child } = workspace({ packages: ['packages/*'] });
    expect(findRoot(child)).toBe(root);
    expect(findRoot(root)).toBe(root);
  });
  it('stops at the nearest workspace even when its patterns do not match', () => {
    const { root, child } = workspace(['packages/**']);
    writeFileSync(join(root, 'packages/package.json'), JSON.stringify({ workspaces: ['apps/*'] }));
    expect(findRoot(child)).toBeNull();
  });
  it('does not include hidden workspace directories by default', () => {
    const { child } = workspace(['packages/*'], 'packages/.hidden');
    expect(findRoot(child)).toBeNull();
  });
  it('does not recurse through deeply nested brace patterns', () => {
    const { child } = workspace(['{'.repeat(10000) + 'client' + '}'.repeat(10000)]);
    expect(findRoot(child)).toBeNull();
  });
  it('surfaces malformed manifests instead of silently ignoring them', () => {
    const { root, child } = workspace(['packages/*']);
    writeFileSync(join(root, 'package.json'), '{');
    expect(() => findRoot(child)).toThrow(SyntaxError);
  });
});
