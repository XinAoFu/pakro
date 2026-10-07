---
aside: true
---

# Packages

mise installs packages natively, without pip/npm/conda in the process tree.

## Quick start

```sh
# declare a runtime and packages
mise use python@3.10.14+numpy@1.26.4

# bare names bind to the sole package runtime
mise install six@1.17.0

# go and maven projects use their own go.mod/pom.xml; mise routes storage
```

## Configuration

```toml
[tools]
python = "3.10.14"
node = "22.14.0"

[packages."python@3.10.14"]
numpy = "1.26.4"
scipy = "1.11.4"

[packages."node@22.14.0"]
typescript = "5.8.2"
```

Versions are exact by design (ADR-0004). Prefix keys like `python@3.10` are
accepted; the resolver pins the concrete version in the lockfile.

## How it works

- **Resolution**: [pubgrub](https://pubgrub-rs.github.io/pubgrub/) with
  ecosystem-specific version adapters (pep440 for python, node-semver for npm)
- **Storage**: a global content-addressed store at
  `~/.local/share/mise/store/` — identical packages are stored once and
  shared across projects
- **Materialization**: python projects get a standard venv shape at
  `.mise/venvs/<runtime>/`; node projects get pnpm-style `node_modules` with
  per-file hardlinks into the store
- **Locking**: `mise.packages.lock` records the full resolved graph with
  digests, ensuring reproducible installs

## Source builds

When no prebuilt wheel matches your platform, mise falls back to building the
sdist locally (`pip wheel --no-deps`). A warning is shown before the build
runs, since it executes the package's build code.

## Store management

```sh
mise install  # triggers daily gc
mise store ls # summary (entries, sizes, ecosystems)
```

## Go and Maven

go.mod and pom.xml remain the package declarations. mise sets GOMODCACHE and
maven.repo.local to point at the shared store, so all projects share cached
modules and artifacts.

## Scope

- python: PyPI (wheels + sdist fallback)
- node: npm registry (tarballs + native module rebuild)
- go: go.mod + GOMODCACHE routing
- java/maven: pom.xml + local repo routing
