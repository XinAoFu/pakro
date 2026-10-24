# pakro

**2026-10-25用户确认：P1 本机 macOS arm64 产品验收通过，自动多进程运行器禁用。跨平台、完整ISAT业务和后续阶段边界保持，P2尚未开始。**
面向 macOS 和 Linux 的开发环境命令行工具，负责语言运行时、项目依赖包、环境变量和任务执行。

## 目录

| 目录 | 职责 |
| --- | --- |
| `src/` | 程序入口、共享配置和服务、基础库、构建脚本及配置 Schema |
| `cli/` | 终端命令、参数、Shell 集成、输出界面、补全和 manpage |
| `runtimes/` | 语言与工具的版本解析、下载、安装、存储布局、链接和注册表 |
| `packages/` | 包的索引、依赖解析、下载、内容存储、环境物化及宿主包管理 |
| `tests/` | benchmarks、端到端测试、测试脚本和夹具 |
| `packaging/` | 打包、安装、签名、发布配置及脚本 |
| `docs/` | 仅保留 `devlog/` 中的需求、架构、计划、决策、开发日志和交接记录 |

Cargo、pakro 和 Nix 的根入口，以及工具自动发现所需的隐藏配置，保留在根目录。

## 命名与当前状态

主命令、Cargo package 和 library 均为 `pakro`；自有 crate 使用 `pakro-*`。项目配置为 `pakro.toml`，工具锁为 `pakro.lock`，包图目前仍使用 `pakro.packages.lock` 边车；项目状态为 `.pakro/`，早期设置为 `.pakrorc.toml`。公开环境变量使用 `PAKRO_*`，内部协议使用 `__PAKRO_*`。

当前提供本地源码构建。独立发行仓库、下载域名和信任根尚未建立，产品自更新和默认远端自动下载暂不可用；保留的上游链接仅用于真实第三方来源、开发设施或历史验证资料。

截至2026-10-24，本机macOS arm64已完成Python共享语言库、目录选择、精确安装、wheel依赖图与共享布局、关联查询及指定删除；随后补齐限定registry纯Python源码包的无pip/uv构建，真实ISAT已由用户完成安装、环境选用及基础启动验收。严格包锁回放、配置/锁/关联联合事务、编译器源码后端、其他来源、Conda和Java/Node新架构仍未完成，Linux/PyPy完整矩阵未验收。

项目已确认先完成Python、再Java、最后Node的P1–P9顺序，整个项目停用dev-flow skill。[当前计划](docs/devlog/PLAN.md)记录P1本机macOS arm64产品验收通过及运行器禁用的收尾状态，P2尚未开始；当前能力、验收边界及R00–R45逐项对照见[架构阶段交叉盘点](docs/devlog/ARCHITECTURE-REVIEW-2026-10-24.md)。

## 开发

在仓库根目录执行：

```sh
cargo build --all-features
./target/debug/pakro --help
cargo test --workspace --all-features
cargo clippy --workspace --all-features --all-targets -- -D warnings
```

本地构建后，将 `target/debug` 加入当前终端的 PATH，再执行项目任务：

```sh
export PATH="$PWD/target/debug:$PATH"
pakro run build
pakro run test:unit
pakro run test:e2e --all
pakro run lint
```

指定端到端测试时，将测试名称传给 `pakro run test:e2e`；测试由统一入口运行。

生成 CLI usage 规格、Bash/Zsh/Fish 补全、manpage 和配置 Schema：

```sh
pakro run render
```

Schema 生成器使用 Rust；仓库不再维护文档网站或文档 Node 依赖。

当前开发状态从 [交接记录](docs/devlog/HANDOFF.md) 开始阅读，详细档案位于 [docs/devlog](docs/devlog/)。

项目使用 [MIT 许可证](LICENSE)。
