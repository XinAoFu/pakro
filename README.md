# mise

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

Cargo、mise 和 Nix 的根入口，以及工具自动发现所需的隐藏配置，保留在根目录。

## 开发

在仓库根目录执行：

```sh
cargo build --all-features
target/debug/mise --help
cargo test --workspace --all-features
cargo clippy --workspace --all-features --all-targets -- -D warnings
```

项目任务也可通过 mise 执行：

```sh
mise run build
mise run test:unit
mise run test:e2e --all
mise run lint
```

指定端到端测试时，将测试名称传给 `mise run test:e2e`；测试由统一入口运行。

生成 CLI usage 规格、Bash/Zsh/Fish 补全、manpage 和配置 Schema：

```sh
mise run render
```

Schema 生成器使用 Rust；仓库不再维护文档网站或文档 Node 依赖。

当前开发状态从 [交接记录](docs/devlog/HANDOFF.md) 开始阅读，详细档案位于 [docs/devlog](docs/devlog/)。

项目使用 [MIT 许可证](LICENSE)。
