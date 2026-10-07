# Workspace lookup for BLE installation tooling

This helper is derived from Square's `find-yarn-workspace-root` 2.0.0
(https://github.com/square/find-yarn-workspace-root), under the retained Apache-2.0
license. The only consumer is `patch-package`, used by the optional Matter BLE
native packages during installation.

The local override replaces micromatch with its maintained picomatch matcher.
It preserves workspace arrays, `workspaces.packages`, nearest-root traversal,
ordered exclusions and re-inclusions, and the original null result outside a
workspace. It removes the transitive braces dependency affected by
GHSA-vfj7-8cjw-p6xm without changing or disabling BLE packages.

Keep the override until the upstream helper or patch-package dependency path
removes the vulnerable package. Run `npm test` and `npm ci` when updating it.

Validation: clean npm 10 installation, 256 Matter tests, TypeScript checks,
a real patch-package CLI application against a temporary fixture, and npm audit
(zero vulnerabilities) passed on macOS. Bluetooth packages remain present in
the dependency graph. Native BLE commissioning and radio access require
separate device verification; the Mac tests use installation scripts disabled.

The package retains its upstream name and version to satisfy patch-package's
existing semver contract; the lockfile explicitly identifies the local source.
This is not a relabeled braces package: braces and micromatch are absent.
