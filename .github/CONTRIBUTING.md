# Development

Read [AGENTS.md](../AGENTS.md), the [architecture](../docs/Pakro主架构文件与Agent规则.md)
and the [current plan](../docs/PLAN.md) before changing this repository.

Build and test directly with Cargo. Run a retained CLI test through
`bash tests/e2e/run_test <test-path>` after building the executable.
The [README](../README.md) lists the commands and current capability limits.
Do not invoke the removed product task runner to build or test Pakro.

Long checks are run by the user in a terminal. Each agent check group has a
600-second wall-clock limit; the automatic multiprocess runner remains disabled.
