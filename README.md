# Pakro

Pakro 用一个共享库管理语言运行时、依赖包和项目选择。项目保留源码和配置，相同的Python和兼容包由多个项目共用。

当前开发主线是CPython：精确安装、目录选用、共享依赖、统一精确锁、显式更新和项目关联管理。支持范围、平台证据及阶段审查结论以[当前计划](docs/PLAN.md)为准。

## 使用逻辑

开发者手动维护Pakro配置，Pakro负责读取、校验、安装和选用。用户项目的`pyproject.toml`与`requirements.txt`不作为依赖来源；第三方包自身的依赖和构建说明仍会读取。进入目录只选已有内容，缺项需要你明确执行安装。

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
pakro deps install python
```

包声明也可以使用表形式，例如`idna = {version = "3.10", source = "pypi", marker = "python_version >= '3.13'"}`。`extras`可列出包的可选功能；marker按实际选用的CPython判断，未生效声明仍保留在锁的意图中。直接版本必须精确。

安装成功后，目录接入会选中对应Python和包，可以正常执行`python`或包提供的命令。首次受支持安装生成统一`pakro.lock`；已有完整锁按原精确结果恢复，不重新挑版本。显式要求严格恢复：

```sh
PAKRO_LOCKED=1 pakro install
```

严格恢复遇到缺锁、不完整或声明不匹配时会拒绝。普通安装遇到已有锁与声明不匹配也会保留原选择。手动修改配置后，明确预览和更新：

```sh
pakro install --update-lock --dry-run
pakro install --update-lock
```

预览展示直接声明变化、可确认的本地状态和未求解部分。实际更新优先保留兼容的旧版本、来源与制品，成功后共同提交锁和项目选择；准备失败保留原选择。它只处理当前配置，与严格锁定、临时包参数和字节码恢复互斥。损坏或不支持格式的锁须明确报错，不以删除锁作为修复步骤。

## Python 源码构建

原生包构建范围为 setuptools、scikit-build-core/CMake/Ninja、meson-python，以及 maturin、setuptools-rust。编译器、SDK和系统开发库由用户准备，缺少时会列出缺项；Pakro不调用系统包管理器补装。Rust源码须带完整`Cargo.lock`，Cargo使用已核验的固定输入离线构建。

需要从官方源码构建解释器时，在`pakro.toml`中明确选择完整版本：

```toml
[tools]
python = { version = "3.13.16", source = "source" }
```

随后仍用`pakro install`。工具默认从已安装程序中查找，也可在`[settings.python.build.tools]`指定`cc`、`cxx`、`ar`、`linker`、`make`等实际路径，在`[settings.python.build]`指定`sdk`或`sysroot`。源码解释器使用普通release/GIL配方，不默认安装pip；当前运行时配方保留普通标准库，省略CPython自身回归套件和测试扩展，配方事实可在`pakro installs ls`中查看。

工具链、SDK、配方和实际输出分别进入构建记录；已有锁恢复时不静默换工具或接受不同产物。构建继续受文件、网络和子进程清理边界约束。各平台实际完成情况以PLAN为准。

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

预览确认后去掉`--dry-run`才执行删除。目标有多个实例时需使用`pakro installs ls`提供的完整`--instance`；仍被项目、依赖或在途操作使用时拒绝删除。

离开目录只撤回终端选择。要解除不再使用的项目，从`pakro project ls`取得其完整ID，再执行：

```sh
project_id='替换为要解除登记的完整项目ID'
pakro project forget "$project_id" --dry-run
pakro project forget "$project_id"
```

forget解除登记和关联，项目文件与共享内容继续保留；原目录已经删除也可以按ID操作。移动项目时，先在新路径成功安装或选用，再forget旧ID。

显式预览和回收确定不用的受管内容：

```sh
pakro prune --dry-run
pakro prune
```

仍被引用的内容正常保留。正在使用、待恢复、损坏或无法确定状态的内容会说明原因。普通安装不会自动回收；下载缓存继续由缓存命令管理。

删除未完成时，输出会分别说明已经删除和仍保留的内容，并提供操作ID。续行只处理该操作原来的候选，不顺便加入新垃圾：

```sh
operation_id='替换为未完成删除输出的完整操作ID'
pakro prune --resume "$operation_id" --dry-run
pakro prune --resume "$operation_id"
```

指定卸载和prune共用删除及续行机制。`prune`、`uninstall`按本机库和操作记录工作，不要求当前项目配置可读；根级`-n`同样只预览，包括续行。

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
bash tests/scripts/p3-manual-check.sh build
cargo fmt --all -- --check
cargo run -p pakro-settings --example render-schema
```

P3定向测试和生命周期验收分别使用同一脚本的`unit`、`lifecycle`组；需要按脚本提供两份指定的真实发行归档。长验证由用户终端执行，每组上限600秒。实际执行结果见PLAN，不将命令示例视为已经通过。

架构与工作规则见[主架构文件](docs/Pakro主架构文件与Agent规则.md)，Agent入口见[AGENTS.md](AGENTS.md)。Git提交和推送分别依据用户当次明确授权；验证规则和原基线证据见PLAN，不把旧测试通过当作当前清理后的通过。

项目使用[MIT许可证](LICENSE)。
