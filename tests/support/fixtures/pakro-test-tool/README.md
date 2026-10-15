# pakro-test-tool

A simple test tool for validating pakro lockfile functionality across multiple platforms and backends.

## Purpose

This tool is designed to test pakro's lockfile generation and validation features by providing:

- Simple Node.js CLI script (no compilation needed)
- Multi-platform support (linux-x64, linux-arm64, macos-x64, macos-arm64, windows-x64)
- Environment variable inspection for pakro testing
- Platform information reporting

## Usage

```bash
# Direct usage
./bin/pakro-test-tool

# Run the local fixture with an explicit test variable
PAKRO_TEST_FIXTURE=1 ./bin/pakro-test-tool
```

## Output

The tool prints:

- Version information from package.json
- Current platform (OS and architecture)
- Command line arguments
- PAKRO\_\* environment variables

## Testing Scenarios

This tool enables testing of:

- Multi-platform lockfile generation
- Platform-specific metadata collection
- Backend compatibility (github, ubi, aqua, http)
- Frozen installs with lockfile validation
- Incremental updates and platform additions

## Implementation

- **Main script**: `bin/pakro-test-tool` (Node.js with shebang)
- **Windows shim**: `bin/pakro-test-tool.cmd` (batch file)
- **Metadata**: `package.json` (version and package info)

## Integration

The local fixture lives at `tests/support/fixtures/pakro-test-tool/`. Tests use
its executable and metadata to exercise lockfile behavior. A separate Pakro
fixture release repository has not been established; use the checked-in fixture
for local development.
