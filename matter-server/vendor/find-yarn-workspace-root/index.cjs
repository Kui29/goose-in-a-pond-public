// Modified from Square find-yarn-workspace-root: use picomatch instead of micromatch.
'use strict';

const fs = require('fs');
const picomatch = require('picomatch');
const path = require('path');

module.exports = findWorkspaceRoot;

/**
 * Adapted from:
 * https://github.com/yarnpkg/yarn/blob/ddf2f9ade211195372236c2f39a75b00fa18d4de/src/config.js#L612
 * @param {string} [initial]
 * @return {string|null}
 */
function findWorkspaceRoot(initial) {
  if (!initial) {
    initial = process.cwd();
  }
  let previous = null;
  let current = path.normalize(initial);

  do {
    const manifest = readPackageJSON(current);
    const workspaces = extractWorkspaces(manifest);

    if (workspaces) {
      const relativePath = path.relative(current, initial);
      if (relativePath === '' || matchesWorkspace(relativePath, workspaces)) {
        return current;
      } else {
        return null;
      }
    }

    previous = current;
    current = path.dirname(current);
  } while (current !== previous);

  return null;
}

function extractWorkspaces(manifest) {
  const workspaces = (manifest || {}).workspaces;
  return (workspaces && workspaces.packages) || (Array.isArray(workspaces) ? workspaces : null);
}

function readPackageJSON(dir) {
  const file = path.join(dir, 'package.json');
  if (fs.existsSync(file)) {
    return JSON.parse(fs.readFileSync(file, 'utf8'));
  }
  return null;
}

function matchesWorkspace(relativePath, patterns) {
  let included = false;
  let excluded = false;
  let allNegative = patterns.length > 0;
  for (const pattern of patterns) {
    const matcher = picomatch(String(pattern), {}, true);
    const negative = matcher.state.negated || matcher.state.negatedExtglob;
    const matched = matcher(relativePath);
    if (!negative) allNegative = false;
    if (negative && !matched) excluded = true;
    if (!negative && matched) {
      included = true;
      excluded = false;
    }
  }
  return (included || allNegative) && !excluded;
}
