# Pakro

Pakro 用一个共享库管理语言运行时、依赖包和项目选择。项目保留源码和配置，相同的Python和兼容包由多个项目共用。

当前核心是Python：精确安装、目录选用、wheel和限定纯Python源码包、关联查询、指定删除，以及统一精确锁和严格恢复。原P1/P2已通过本机macOS arm64验收；旧代码删剪已完成静态检查，本轮修改后的编译和行为验收尚未完成。Linux和其他平台组合、编译器源码后端、Conda直接管理及Java/Node新架构仍未完成。完整范围见[当前计划](docs/PLAN.md)。

## 使用逻辑

开发者手动维护配置，Pakro负责读取、校验、安装和选用。配置不合理或与精确锁不一致时明确报错，不自动改配置、不静默修锁。进入目录只选已有内容，缺项需要你明确执行安装。

先将本机构建的`pakro`放入PATH。在Zsh的启动配置中加入一次：

```sh
eval "$(pakro activate zsh)"
```

Bash使用`pakro activate bash`。这是Shell接入，不是每个项目都要创建或激活一套环境。

在项目目录手动编写`pakro.toml`。下面展示配置格式，实际版本必须有匹配当前平台的制品；有多个发行build时需明确build：

```toml
[tools]
python = "3.13.16+20261013"

[packages."python@3.13.16+20261013"]
idna = "3.10"
```

显式安装配置中的Python和包：

```sh
pakro install
# 或安装项目声明的Python依赖：
pakro deps install
```

安装成功后，目录接入会选中对应Python和包，可以正常执行`python`或包提供的命令。首次受支持安装生成统一`pakro.lock`；已有完整锁按原精确结果恢复，不重新挑版本。显式要求严格恢复：

```sh
PAKRO_LOCKED=1 pakro install
```

锁缺失、不完整或与当前声明不一致时会拒绝。手动修改配置后的重新锁定和旧关联释放流程仍待确认，不要把删除锁文件当作自动修复步骤。

## 常用查询与删除

```sh
pakro current
pakro where python
pakro which python
pakro env --json
pakro project status
pakro project ls
pakro installs ls
```

这些本地查询不会偷偷下载或安装。`where`看Python实际安装根，`which`看当前命令入口；语言存在不代表项目全部包都已就绪。

只删除明确指定的安装对象，例如：

```sh
pakro uninstall python@3.13.16 --dry-run
pakro uninstall --package 'python@3.13.16+idna@3.10' --dry-run
```

预览确认后去掉`--dry-run`才执行删除。目标有多个实例时需使用`pakro installs ls`提供的完整`--instance`；仍被项目、依赖或在途操作使用时拒绝删除。卸载不修改项目配置，离开目录也不释放持久引用。批量prune尚未完成。

## 构建与项目文件

当前从本地源码构建；独立发行下载入口和自更新尚未建立：

```sh
cargo build --all-features
./target/debug/pakro --help
```

| 路径 | 内容 |
| --- | --- |
| `src/` | 程序入口、配置、三库和核心程序 |
| `cli/` | 命令、Shell接入和终端输出 |
| `runtimes/` | 语言发行获取、校验和运行时适配 |
| `packages/` | 包来源、依赖求解、构建、校验和共享布局 |
| `tests/` | 回归用例、CLI测试和验证脚本 |
| `packaging/` | 工程打包与发布资源 |
| `docs/PLAN.md` | 唯一进度和实施计划 |

开发检查直接使用Cargo和Shell，不调用产品任务命令：

```sh
cargo test --workspace --all-features
cargo fmt --all -- --check
cargo run -p pakro-settings --example render-schema
# 先完成构建，再指定一个仍保留的CLI用例：
bash tests/e2e/run_test cli/test_f1_directory_selection
```

需要真实发行归档的用例须按脚本提供输入。长验证由用户终端执行；本轮尚未执行上述构建和行为测试。GitHub仅保留手动调用的基础检查入口，未运行CI或发布。

架构与工作规则见[主架构文件](docs/Pakro主架构文件与Agent规则.md)，Agent入口见[AGENTS.md](AGENTS.md)。Git提交和推送分别依据用户当次明确授权；验证规则和原基线证据见PLAN，不把旧测试通过当作当前清理后的通过。

项目使用[MIT许可证](LICENSE)。
