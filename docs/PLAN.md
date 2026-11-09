# Pakro Python 完成计划：P2 之后

更新：2026-11-09（America/New_York）。架构入口：[主架构与Agent规则](Pakro主架构文件与Agent规则.md)。

## 当前状态与本轮授权

- 已完成：P1/P2、P3/P4（既定 macOS arm64 验收）、P6 及四项收尾、P8 前两批。P8 第三批实施中，P8/P9 均未整体完成。历史失败过程不再逐条保留，只保留结论与证据入口。
- 本轮授权以用户第二轮六步指令、后续范围决定及 2026-11-09“继续”为准：实施主会话亲自编写代码及执行测试，check 只读审核、不运行测试；旧 PLAN 分工与授权不适用。只在本机 macOS arm64 验证，不使用服务器或 Docker。第 6 步在全部前置条件及提交分组审核通过后允许本地分批 commit；禁止 push、分支操作、amend、rebase、reset 和修改 git config。本轮放弃多包环境脚本和 GDAL 四种新语法；第 1、2 步已获 check 通过，第 3 步最后一次回收按用户新决定延后到删除简化后验证，第 4 步已审核通过，现执行第 5、6 步；本轮不修改删除实现。
- 每组编译加测试限 600 秒。全部测试按用户指定四组串行执行，保持 `RUST_TEST_THREADS=1`；某组超时允许按下一级模块继续拆分，每组额度不变，覆盖清单按目标和测试名去重。其他专项验证超时仍须停止，不以拆分或重试绕过上限。
- 本轮原始输出和输入保存在 `target/p8-closeout-20261108/`，沿用 `target/`、`--all-features` 和 CI unit 构建参数；本轮临时目录按 owner 清理，不碰用户真实库与项目。罕见并发防护新增 0 行。
- 生产源码前遵守 AGENTS 四条简化规则：不为推测的罕见场景扩建防御；外部输入在入口校验一次；一次性逻辑就地书写；不随意拆函数、不滥用 try/catch。

### 最新接续决定：本轮提交，下一轮简化删除（2026-11-08）

- 第 3 步不拆分、不再重跑完整用例。除最后一次回收外，原断言均已走过；最后一次回收延后到删除简化完成后验证。2026-11-04 closeout 第二段已有同样最终回收的通过记录，见 `target/p8-closeout-20261104-item3-round1/`；该历史结果不改写本次 600 秒中止为父用例通过。用户明确允许据此继续第 4–6 步。
- 本轮按已定分组执行全部测试，然后整理 PLAN、审核提交分组并本地分批提交。本轮不修改删除代码；第 7–12 项不开始。
- 下一轮取消每删一步保存进度并强制写盘、按操作号 `pakro prune --resume` 续删，以及续删时的逐项核对。
- 下一轮保留删除前确认无项目引用并取得锁；每个对象先标为不可用并写入磁盘一次。只删除 Pakro 清单内的文件；发现清单外文件、被替换的目录或符号链接时停止，不继续删除。
- 中断留下的对象保持不可用；下次 `pakro prune` 直接清理残留，清单中已不存在的文件视为已删除。下一轮按新设计重做 P4 阶段的删除验证，并实测回收用时。
- 这项决定替代架构文件与本 PLAN 旧的按操作号续删设计，但实施推迟到下一轮。本轮现有实现和测试仍按当前代码验证，不提前删除 `--resume` 或改变删除行为。

### 第 4 步分组验证与超时测试修复（2026-11-08，已通过）

| 对照 | 测试数 | 通过 | 失败 | ignored | 原始输出 |
| --- | ---: | ---: | ---: | ---: | --- |
| 修改前完整分组 | 5070 | 4983 | 3 | 84 | `target/p8-closeout-20261108/grouped-before/` |
| 第一次修改后完整分组 | 5074 | 4986 | 1 | 87 | `target/p8-closeout-20261108/grouped-after/` |
| 超时测试修复后完整分组 | 5074 | 4987 | 0 | 87 | `target/p8-closeout-20261108/timeout-test-fix/full-suite/`；第四组为上一级 `group4/` |

- 统计按目标及测试名去重，覆盖 5074/5074，无遗漏或计数冲突，详见最终 `coverage.json` 与 `comparison.json`。新增 5 项，按用户取消 GDAL 成功验证的决定移除 1 项。没有开启自动接受快照或强制通过的环境设置。
- 最终四组用时分别为 62.50、57.04、491.85、93.47 秒，每组编译加测试均在 600 秒内。第四组库 2142 通过/7 ignored、主程序 96 通过/2 ignored、文档 1 通过/2 ignored。主程序已重新编译；P4 真实主程序用例另行 1 通过、7.32 秒，不重复计入四组。check 已通过本次修复及完整覆盖核对。
- 修改前的代理与排序失败按用户授权处理后通过；P4 缺少主程序输入的问题已补 ignore 原因并单独验证，属于测试标注问题。前序失败与准备修正记录保留在下节和原输出中。
- 第一次修改后唯一失败是 `the_fixed_probe_timeout_is_propagated`：500 毫秒包括分配、解包、启动全过程，超时报错却不能保证脚本已写出 started；随后读取文件出现 ENOENT。修改前同名测试通过，单独复核也通过，未用复核覆盖首次失败。check 的调查确认原有测试缺少时序前提；首次具体超时阶段无法还原，未发现本轮产品逻辑回归证据，不能归因于已确认的机器负载。
- 用户授权调查后，临时诊断得到 2142 通过/7 ignored，行为 37.92 秒、含编译 107.17 秒；该次进入观察时剩约 403.522ms，spawn 0.626ms，started 存在并在等待阶段正常超时。原始数据在 `probe-diagnosis-1/`，只证明该次路径。诊断代码经 check 核对后已全部撤回，修改前副本和差异保留。
- 用户随后授权先修复测试。本次只修改 `runtimes/src/plugins/core/python/native/extract/tests.rs` 和 `Cargo.toml` 的测试依赖：为已有 Tokio 开启 dev-only test-util；准备沿用普通测试的 5 秒真实时间预算，保留原 owner 绝对截止时间。测试阻止虚拟时钟自动推进，等待真实 started/PID，在原截止时间前 500ms 确认仍 Pending 且进程存活；跨过原截止时间一个毫秒刻度后等待原 future 返回并确认 PID 已退出。没有修改生产截止时间、生产实现或删除代码。
- 修复相关模块 57 通过/6 ignored，随后最终四组通过。单组准备、编译与测试均保留实际时间；旧失败、诊断和修复后结果分开保存，不把历史失败抹去。
- 本轮所有新建测试临时目录均已按各 owner 的 uid/device/inode 核对并经 lsof 检查后清理；清理结果在相应 `cleanup-result.json`。target 内的原始输入、输出与完整清单保留，旧轮资源未动。
- shell e2e 本轮未运行。`tests/e2e/run_test` 和 CI 仍提供按单个路径运行的入口，但 `direnv/test_direnv:26`、`lockfile/test_lockfile_upgrade_prune:47`、`fixtures/native_python_transaction.py:182` 等仍引用已删除的 use、upgrade、install-into；`run_all_tests` 注释还使用已删除的 `pakro run`。整目录不能视为可通过的当前测试集，整理留第 11 件。

### 第 5、6 步：文件核对与提交分组（2026-11-09）

- 当前待提交共 224 个文件：126 个已跟踪修改、98 个新文件。第 4 步通过后，除本 PLAN 外没有修改源代码、配置、测试或快照；非文档文件的修改时间均不晚于最后验证。
- 71 个新 `.snap` 全部对应当前 52 个已通过的测试，拟全部提交。消费者分布：shell 模块 36 个文件；插件配置解析 1 个；Pakro TOML 配置解析 23 个；配置加载 1 个；direnv/env_diff 9 个；SHA256 输出 1 个。配置中的多个断言按 Insta 自动编号核对，包含 test_env 的首个内联断言，因此其文件从 env-2 开始。头部的旧源码路径只作历史元数据保留，当前消费者已按实际源码定位。
- 快照正文已与现有断言和最终测试结果核对；direnv parse 中的 `/private/var/folders/...` 是源码固定编码输入解出的常量，不是本轮临时目录。未发现未能确认消费者的快照，未改写已测试内容。
- `.config/settings.toml` 由 pakro-settings 构建器读取，新增 Conda 渠道默认值并修正原有 Python 构建项排序；排序测试及全部分组通过，纳入功能提交。`.gitignore` 将 Snapcraft 的 `*.snap` 限于根目录 `/*.snap`，保留对 `.snap.new` 和 target 的忽略，使上述测试快照可跟踪，随快照提交。
- 新 `tests/e2e/cli/test_ls_remote_package` 静态核对为现有 ls-remote 包版本查询的本地 HTTP 用例；代码与断言对应当前接口，纳入测试提交，但按本轮要求没有运行，运行验证留第 11 件。所有待提交文件均为已确认的源代码、配置、测试输入或文档；没有归属不明而排除的文件。target、临时目录和密钥不提交。
- 提交方案按整文件分为四笔工作提交，再追加一笔 PLAN 提交记录：① 固定锁输入及共享提交/恢复基础（11 文件）；② Conda/PyPI、脚本、查询及现有引用回收适配（90 文件）；③ 测试修复、验证输入、71 个快照及忽略规则（120 文件）；④ README、架构文件既有改动及本 PLAN（3 文件）；⑤ 仅 PLAN，记录前四笔工作提交的哈希。最后一笔自身哈希在最终回复和 git log 中给出，避免文档自引用。
- `closed.rs` 的清理和 Conda 混合图新增处于同一文件，随第二笔提交，不把它伪称为独立纯清理提交。第一笔还引用第二笔的 Conda 类型与入口，不能单独编译；前两笔尚未包含第三笔的测试文件，不能单独编译完整测试目标。第三笔齐备后代码、配置、测试、快照集合才与已验证集合一致；没有对中间提交宣称单独验证通过。依赖顺序为 ①→②→③→④→⑤。
- 第 5 步已获 check 第 2 轮通过（仅文档更正，提交分组不变）。第 6 步按批准清单逐路径 git add 并本地分批 commit；提交过程中不修改已测试代码，不运行 push、分支操作、amend、rebase、reset 或 git config 修改。

已存在的提交已核实，不计作本次新提交：

| 哈希 | 标题 | 已包含内容 |
| --- | --- | --- |
| `38a1456c7` | `refactor: remove unused Python runtime modules` | 删除无消费者的运行时模块，保留发布和源码构建所用的 target 投影；当时明确保留 Conda-mixed closed.rs 的改动待后续提交。 |
| `2540b4028` | `test: correct Python install and build fixture setup` | 安装测试补初始锁发布及 Python 来源输入，校验锁保持；构建测试规范化项目路径。 |

### 收尾第二轮六步（2026-11-08，前序实施记录）

- 开工前执行一次 `cargo test --workspace --all-features`，使用 CI 的 `CARGO_INCREMENTAL=0`、`CARGO_PROFILE_DEV_DEBUG=0`、`CARGO_PROFILE_TEST_DEBUG=0`，提供已保留的固定 Python 归档。600.04 秒达到上限，已停止本组进程，未重试。已完整结束的 aqua-registry 为 138 通过；原始输出共记录 2981 条通过条目（含 2 条重复子用例输出）、1 条失败、59 条 ignored，不作为 workspace 最终统计。超时位置为 `programs::install::lock_document::tests::p3_update_overlay_replaces_old_python_results_and_retains_foreign_native_data`。证据：`baseline-workspace/command.json`、`output.txt`、`result.json`、`observed-counts.json`。
- 按用户接续决定补跑修改前全部测试：`--list` 得到 5070 条；四个指定组及 Cargo 遇失败后尚未执行的目标全部完成，按可执行目标及名称去重为 4983 通过、84 ignored、3 失败，覆盖 5070/5070，无遗漏或计数冲突。`grouped-before/coverage.json` 保存每项观察及去重结果，`grouped-before/list/inventory.json` 为原始清单。第三组 472.22 秒，所有实际执行组均未超过 600 秒。第二组与第三组共同包含 `programs::remote_packages::tests::metadata_versions_are_unique_and_sorted_by_source_version_rules`，总数只计一次。
- 修改前失败原始记录保留：① `pakro_cache_core :: tests::retries_reqwest_errors_wrapped_by_blob_stream_io` 收到 HTTP 502；依用户最新授权，仅对子进程设置 `NO_PROXY=127.0.0.1,localhost` 单独运行一次后 1 通过，记为本机代理影响，未改代码或系统代理，证据 `small-items/cache-no-proxy/`。② 设置文件排序失败已按授权处理：仅重排 `python.build.*`、`python.compile`、`python.conda.channels` 的原有块，字段及值不变；排序测试 1 通过，证据 `small-items/settings-order/`。③ P4 缺少主程序输入的标注问题见下条。修改前输出仍在 `grouped-before/group1/`、`group1-remaining/`、`group3/`，不改写历史统计。
- 测试准备修正单独保留记录：6 个原生执行用例初次缺少 `PAKRO_TEST_BINARY`，另有短临时目录与属组准备问题导致 3 个用例没有形成预定测试条件（Unix socket 路径过长；wheel 属组使请求的 02700 实际被系统清成 0700）。提供已编译主程序、改用自有短目录并设为当前有效属组后，只重验受影响用例，分别 7 通过和 2 通过，仍在各自原连续 600 秒窗口内；未修改源码或断言。原失败与修正后输出均在 `grouped-before/`，没有用后次通过覆盖原输出。
- 全部测试的标准准备：先在同一 `target/`、`--all-features` 及 CI unit 构建参数下执行 `cargo build --bin pakro --all-features`，为测试设置 `PAKRO_TEST_BINARY` 为本次生成的 `target/debug/pakro` 绝对路径；使用 owner 记录的短临时目录与当前有效属组，保留固定 `PAKRO_TEST_PYTHON_ARCHIVE` 输入，并对子进程设置 `NO_PROXY=127.0.0.1,localhost`。不改系统代理、不改变 `.cargo/config.toml` 的串行设置、不再改其他测试标注。先执行 `cargo test --workspace --all-features -- --list` 保存完整清单，再依次执行下列四组，每组编译与测试合计不超过 600 秒，超时按用户授权向下一级模块拆分。按测试目标及完整名称核对覆盖，一个不漏、重复只计一次。P4 另设 `PAKRO_TEST_MAIN_BINARY` 指向同一主程序，用 `--ignored` 单独执行 `p4_real_main_prune_respects_two_projects_and_completed_resume`。
  1. `cargo test --workspace --all-features --exclude pakro`
  2. `cargo test -p pakro --all-features -- packages::`
  3. `cargo test -p pakro --all-features -- programs:: libraries::`
  4. `cargo test -p pakro --all-features -- --skip packages:: --skip programs:: --skip libraries::`
- 用户指定的 P4 标注问题已修正：只补 `#[ignore]` 及必需输入说明，另行编译主程序并设置 `PAKRO_TEST_MAIN_BINARY` 执行原用例，1 通过、0 失败、0 ignored，行为 8.89 秒。构建和测试命令执行时间合计 109.66 秒，共用同一 600 秒窗口；原始输出在 `p4-annotation/`。按用户决定记录为测试标注问题，不作为产品失败。
- 开工前现有 Conda 脚本测试已执行，包括原始 PROJ 脚本和所有 ignored 用例：主程序构建成功，7 通过、0 失败、0 ignored，含编译整组 128.54 秒，行为测试 21.34 秒。证据：`baseline-conda-scripts/`。
- 第 1 步做法已获 check 开工前回复“可以”。顺序依据为 Conda `conda/activate.py` 的 `_get_activate_scripts`（907–919 行）和 `_get_deactivate_scripts`（921–933 行）：激活路径正序、撤销路径倒序；本次读取的文件 blob 为 `63b0b7bddc74e2605a4851b91e23e8379450d12f`，链接 `https://github.com/conda/conda/blob/main/conda/activate.py#L907-L933`。
- 已取得并核验原始 `gdal-3.13.3-np2py313hc201c6a_5.conda`（2448210 字节，SHA256 `e4cd9e2a92f588d8e6a8ab482d89001d515306260145b55167faf07df6b22ed3`）和 `libgdal-core-3.13.3-h8348644_5.conda`（11953856 字节，SHA256 `264d67c3de0f080022cc78440718cc5ba6465b612ef356b4201ef027d0854fee`），URL 均为 `https://conda.anaconda.org/conda-forge/osx-arm64/` 加对应文件名。原始 API、制品及逐项 URL/摘要/大小在 `inputs/`；GDAL 的脚本由 `libgdal-core` 提供。
- 此前 GDAL 单对原始脚本失败已复现：0 通过、1 失败、0 ignored，含编译 62.20 秒，错误为 `unsupported Conda conditional environment copy`。用户最新已取消 GDAL 成功验收及四种语法扩展，因此移除对应成功断言用例，替换为原始 libxml2-16 的明确拒绝验证；原失败输出保留在 `gdal-single-pair-before/`。本轮不改写原始制品，也不实现通用 shell 解析。
- 完整 GDAL 输入已取得：测试模块复用已有 `prepare_real_records`，新增 ignored 的固定 GDAL/PROJ 准备入口，未改生产求解器；准备测试 1 通过、0 失败、0 ignored，14.47 秒。图中共 73 个制品，无循环；73/73 的原始 URL、SHA256、大小均已核验，压缩体积共 85,476,096 字节，22 个复用、51 个下载，下载组 127.84 秒。完整记录、制品和原始输出在 `inputs/gdal-closure/` 与 `gdal-records-run/`。
- 已检查全部 73 个制品的原始 `info/paths.json`，仅下表三个包带安装/环境脚本。完整原文及清单在 `inputs/gdal-closure/script-audit/`。第一次用 tar 枚举 `libxml2` 包的 payload 返回错误；后来从 `info/paths.json` 得到 2 个文件条目、0 个脚本，检查方式调整后完成全部清单。原错误及后续事实均保留，不把这个检查工具错误定性为制品损坏。

| 带脚本的包 | 原始脚本 | 当前不支持的内容 |
| --- | --- | --- |
| `proj 9.9.0 h51dee2d_0` | 一对 `.sh`，另有配对 fish/csh 版本 | 原始制品安装及 `.sh` 执行已通过，安装输出已确认提示 fish/csh 不执行。 |
| `libgdal-core 3.13.3 h8348644_5` | `gdal-activate.sh`、`gdal-deactivate.sh` | 四种语法扩展现已取消，仍不支持。 |
| `libxml2-16 2.15.4 heb56d2d_1` | 只有 `etc/conda/activate.d/libxml2-split_activate.sh`，没有撤销脚本 | `if test`、不带 export 的赋值、`IFS` 修改、for/while 循环、参数前缀删除、字符串拼接及多变量 unset；含本轮不支持的语法，也不满足每包须有一对 `.sh` 的约定。 |

- 用户已决定：libxml2 是许多 Conda 项目的底层依赖，逐包补脚本写法不能形成合适的通用处理方式，环境脚本处理以后单独设计。本轮不开放多包环境脚本、不补 GDAL 四种语法。带上 libxml2-16 的 Conda 项目目前都会被拒；本轮实际核验制品为 `2.15.4 heb56d2d_1`，因仅有激活脚本且语法不受支持。没有跳过或补造脚本。第 1 步仅补明确拒绝、原始 PROJ 的 fish/csh 提示、固定 60 秒及三种失败路径验证，正式代码仍以约 200 行为限。第 1、2 步已完成审核；第 3 步最后回收按用户决定延后，其余检查结果见下文。
- 缩减后的做法已获 check 回复“可以”。当前脚本及共享监督器生产改动累计 134 行新增/删除，支持边界未放宽；测试中复用原归档校验、锁模型编码、安装入口及 HTTP 镜像。原始 PROJ 安装提示和 libxml2-16 拒绝验证已通过（1 通过、0 失败、0 ignored，行为 194.55 秒；与主程序构建共用窗口总用时约 482 秒），输出见 `small-items/step1-originals/`；自造完好 Conda 包用于非零退出、真实 60 秒超时及向 pakro 发送 SIGINT 的验证，不以缩短执行时间的开关代替实际超时。
- 第 1 步三种失败路径专项通过：1 通过、0 失败、0 ignored，含 Cargo 启动 238.28 秒。使用自造且完整可安装的 Conda 包，脚本分别退出 7、持续运行触发实际 60 秒限制、由测试向 pakro 主进程发送 SIGINT。逐项核对锁、catalog、用户声明字节不变，捕获的主进程、监督进程及脚本子进程全部退出，原已安装 Python 仍可运行；同时覆盖 pre-link、pre-unlink、post-unlink、不成对、无 `.sh`、组合冲突、依赖不允许及未知语法的具名拒绝。测试主动提供变更后的精确锁和声明作为安装输入，失败后比较该次输入，未把它写成自动更新锁测试。原始输出和进程号在 `small-items/step1-failures/`。
- 第 1 步现有脚本与制品提取测试复验：9 通过、0 失败、0 ignored，包含 7 个需要 `--include-ignored` 的实际执行测试，未缩短脚本固定超时；原有更早整体截止时间用例保持。输出在 `small-items/step1-regression/`。
- 第 1 步审核第 1 轮打回：超时前已捕获的 stdout/stderr 被丢弃，报错中的查看输出建议无法执行。已调整现有监督器的最后响应保留有界输出，并在脚本错误中展示；不增加日志协议、配置项或执行器。补充先输出 stdout/stderr 再实际超时的 CLI 断言，已通过后述复验；第 2 轮另有打回，最终修正见第 3 轮记录。
- 第 1 轮打回后的复验保留原结果：三种脚本失败路径断言 1 通过（行为 241.67 秒），实际超时错误已包含先打印的 stdout/stderr；现有脚本/提取 9 通过。但 SIGINT 原始输出出现 watcher 写已关闭管道的 Broken pipe panic，后续共享监督器测试为 5 通过、1 失败、1 ignored：`p6_native_owner_sigkill_stops_grandchild_and_cleans_workspace` 因 panic 跳过目录清理而失败。原连续组约 528 秒结束，未超时，输出在 `small-items/step1-review2/`。此回归不能计为通过；已改为检查写入结果，仅在已收到 owner EOF 且写端为 BrokenPipe 时正常结束以继续原清理，其他错误照常传播，并补 SIGINT 不出现 panic 的断言，修复后主程序构建成功，共享监督器 6 通过、0 失败、1 ignored，包含前述 SIGKILL 清理用例；脚本失败/真实 60 秒超时/SIGINT 专项 1 通过（行为 240.80 秒），SIGINT stderr 为空，超时错误包含先打印的 stdout/stderr，提交状态和进程停止断言保持；现有脚本/制品 9 通过。构建和三组验证依次在同一 600 秒窗口内完成，命令执行共 442.66 秒，输出位于 `small-items/step1-output-fix/`。
- 第 1 步审核第 2 轮打回：大输出后丢弃 `run` future 但保留 session 时，EOF 后的最后响应可能阻塞清理。按 reviewer 要求新增永久回归用例，修改前实际 0 通过、1 失败，错误为私有目录未清理（`small-items/step1-review3/before-drop/`，15.57 秒）。check 已同意缩减处理：恢复原有 EOF 直接清理，不再发送最后响应或特判 BrokenPipe；正常超时仍由 watcher 按原 deadline 停止任务，父侧用既有 2 秒清理时间等待带输出的响应。主动取消/丢弃 future 保持原行为，不承诺取消时输出。修复及后续验证共用本次 600 秒窗口：主程序构建成功；共享监督器 7 通过、0 失败、1 ignored，含新增大输出用例；脚本失败/实际 60 秒超时/SIGINT 专项 1 通过（行为 238.56 秒），超时两路输出可见、SIGINT stderr 为空；现有脚本/制品 9 通过。包含修改前失败命令共 428.13 秒，原始输出在 `small-items/step1-review3/`。第 3 轮已获 check“通过”；第 1 步完成。
- 第 2 步只修改三处循环拒绝报错：Conda 遍历列出重复访问的包及当前访问集合；Python 运行时循环列出 `python` 和已有集合交集；混装 wheel 循环列出被阻塞的待安装包。访问集合、待安装集合可能含非环成员，文案不把全部集合称作确切环成员。未改依赖分析或实现循环安装。`tests/support/fixtures/conda-cycles/` 两份 owa-core/owa-msgs 原记录与用户指定 target 数据逐字节相同；新增测试直接调用现有遍历，从两个原包及外层 consumer 分别进入循环，其他依赖按已处理状态提供，不宣称完整解析或安装成功。相关测试 10 通过、0 失败、3 ignored，含编译 56.29 秒，输出在 `step2-cycle-diagnostics/`；三项 ignored 是公开输入准备入口，本组没有执行。第 2 步已获 check 第 1 轮“通过”；随后在同一 600 秒窗口编译主程序成功，46.93 秒，输出见 `step2-main/`。
- 用户接续决定：旧混装完整测试首次安装改为使用已保存的原始锁，设置 `PAKRO_LOCKED=1`，继续使用保存的 26 个 Conda 原始制品。首次从实时元数据求解并生成锁已于 10 月 29 日实测通过，不再放入这个可重复运行的生命周期测试；此前失败确因实时元数据选择 llvm-openmp 23.1.5，固定输入只有 23.1.4。暖共享、严格冷恢复、加 SciPy 更新失败、用户撤回配置、拒删、forget 和回收均保留；更新步骤仍实时求解，若因元数据变化而非 SciPy 冲突失败则停止，不改断言。仅修改测试及 PLAN，不改生产实现或元数据下载方式。新输出保留在 `resume-locked/`。
- 原始锁接续实跑：只修改 `src/core/programs/install/conda_project/cli_tests.rs` 中首次安装的输入和环境，读取 `PAKRO_TEST_MIXED_ORIGINAL_LOCK`，写入保存原锁后以 `PAKRO_LOCKED=1` 安装，新增原锁字节不变断言；后续更新仍使用原来的非 locked 环境，其他断言及步骤保留。原独立拒绝代理可见性检查保持，它不会生成成功的安装结果。未改任何生产代码。
- 同一 600 秒窗口内主程序构建 1.60 秒成功；测试含编译执行 598.44 秒，整组 600.04 秒被停止，退出码 124，无最终 `test result`，父用例不能计为通过。首次严格安装、原锁不变、暖 A/B 共享、严格冷恢复及原生计算均已完成；SciPy 更新确实因 `scipy==1.14.1 requires numpy<2.3,>=1.23.5` 与 Conda `fixed numpy==2.3.3` 冲突失败，原锁及 catalog 字节保持。用户撤回配置后原运行时正常，被引用 Python 拒删，forget A 后 prune 完成且 B 仍可计算，forget B 成功。停止时最后一次 prune 尚未返回，最终回收断言和完成标记未到；本次不是元数据变化导致的失败。三个停止的 PID 均已核对不存在。命令与原输出：`resume-locked/step3/build/`、`resume-locked/step3/test/`。
- 当时按原停止规则不拆分、不重试、不延长上限，暂未进入第 4–6 步。check 当时通过测试调整与超时停止报告，但未将父用例计为通过；后续用户决定延后最后回收并继续第 4–6 步。本次 `/private/tmp/pk8r-2otjoaee` 及测试子目录已按 owner 核对身份，lsof 确认没有打开文件后清理（11,974 个文件、760 个目录），结果见 `resume-locked/cleanup-result.json`；原始锁、制品与 target 输出全部保留。
- 第 3 步已改旧混装完整测试的 SciPy 本地索引：只保留原始 `scipy-1.14.1-cp313-cp313-macosx_14_0_arm64.whl`，原 URL、SHA256、大小及 METADATA 不变；原始 Simple 响应与仅保留一个候选的本地响应分别保存，其他断言和失败后用户自行撤回声明的步骤保留。输入 23,084,508 字节，SHA256 `e0cf28db0f24a38b2a0ca33a85a54852586e43cf6fd876365c86e0657cfe7d73`，所有 ZIP 成员 CRC 通过，METADATA 与原 sidecar 字节及摘要相同，证据 `step3-inputs/verified.json`。
- 旧完整测试实际运行失败：0 通过、1 失败、0 ignored，含编译 177.81 秒，行为 120.59 秒，未超时。首次安装重新查询公开 Conda 元数据后，选择 `llvm-openmp-23.1.5-hdb3d66b_0.conda`；原完整输入是 `llvm-openmp-23.1.4-hdb3d66b_0.conda`。原测试将未登记的制品请求送至本地 `/input-drift` 并返回 503，所以在首次安装阶段失败，尚未请求 SciPy，也未走到本次冲突、冷暖恢复和回收验证。这是固定测试输入与当前公开求解结果不同，不把它记为 SciPy 冲突验证通过。命令、原输出及保留输入在 `step3-mixed-lifecycle/`。
- 已执行用户第 3 步“因为别的原因失败，停下报告”的停止要求：没有重试、刷新依赖输入、放宽断言或更改生产安装逻辑。check 核对并确认当时停止原因成立。后来按用户决定改为原始锁严格安装；最新验证与提交状态见前述第 4–6 步。
- 本轮涉及文件：第 1 步生产文件为 `packages/src/conda_artifact.rs`、`packages/src/conda_scripts.rs`、`packages/src/conda_scripts/activation.rs`、`src/core/libraries/package/conda.rs`、`src/crates/pakro-util/src/native_build.rs`、`src/crates/pakro-util/src/native_build/supervisor.rs`；测试与输入准备为 `packages/src/conda_scripts/tests.rs`、`src/crates/pakro-util/src/native_build/tests.rs`、`src/core/programs/install/conda_project/script_cli_tests.rs`、同目录 `cli_tests.rs` 和 `dependency_groups.rs`。第 2 步为 `src/core/programs/install/conda_project.rs`、`src/core/programs/install/mixed.rs`、`tests/support/fixtures/conda-cycles/` 两份原记录。第 3 步只改 `src/core/programs/install/conda_project/cli_tests.rs` 的旧混装用例。前置修正涉及 `.config/settings.toml` 和 `src/core/libraries/reclaim/cli_tests.rs`；实施状态只写本 PLAN。
- 停止后的清理已完成：按 `small-items/owner.json` 及六个子目录 owner 核对 uid/device/inode，检查没有进程打开这些目录内的文件后，删除本轮 `/private/tmp/pk8s-wuv9urqv`；共观察 40,780 个文件、2,131 个目录，结果见 `small-items/cleanup-result.json`。target 原始输出、原始制品、输入核验及失败记录保留；用户真实库、项目和旧轮资源未动。
- 首次停止时的临时目录已按 `owner.json` 核对清理，`cleanup-result.json` 保留结果；接续的两个临时目录也已按 `grouped-before/owner.json`、`short-owner.json` 核对身份并清理，清理前没有引用它们的进程，实际结果见 `grouped-before/cleanup-result.json`。原始测试输出、完整制品及脚本原文保留在本轮 target 目录，旧轮资源未动。check 已核对 73 个制品及脚本清单；后续缩减范围及停止状态见本节。

### 收尾第一轮第 1–5 项（2026-11-04，结果保留）

- 修改前测试由实施主会话执行：混合分发映射与 registry 诊断现有测试共 9 通过、0 失败、0 ignored，含 Cargo 启动共 1.64 秒。命令和原始结果：`target/p8-closeout-20261104-baseline/result.json`、`output.txt`。命令中的 mixed_replay 过滤条件未命中用例，不计为该模块通过。
- 第 1 项已获 check 第 1 轮通过：补充混合求解失败时的项目要求、Conda 固定分发版本、候选包依赖说明。新增测试通过普通 Python 包与 Conda 包两组兼容安装及实际导入，随后验证 `consumer==1.0` 要求 `dep>=1,<2` 与 Conda 固定 `dep==2.0` 的冲突；完整包校验保持不变。定向 12 通过、0 失败、0 ignored，含编译共 76.15 秒；原始结果在 `target/p8-closeout-20261104-item1-round1/`。不计为整套 CLI 生命周期通过。
- 第 2 项已获 check 第 1 轮通过：真实 CLI 安装 Conda NumPy 2.3.3 与 PyPI numpy-financial 1.0.0 后，用户添加 SciPy 1.14.1，使用完好的 macOS 14 wheel 触发版本冲突。失败前后锁文件与 catalog 字节相同，`project ls` 登记相同，原先选中的 Python 再次执行导入、财务计算和矩阵计算成功；用户新增声明全过程保持。相关生产更新路径未发现回写依赖声明；保留了会自行还原配置的旧混合测试，本次独立新增验证。主程序构建成功，1 通过、0 失败、0 ignored，整组 239.66 秒；原始输出和本轮专用目录身份在 `target/p8-closeout-20261104-item2-round1/`。此结果证明已安装内容可继续运行；未改变当前配置与旧锁不一致时的目录选择规则；用户接受此时进入目录报错，文案调整留第 10 件。
- 第 3 项已获 check 第 1 轮通过：复用第 2 项本轮专用目录，B 项目严格安装后与 A 使用同一路径；普通 Python 包在被引用时拒绝指定删除。forget A 后 prune 保留全部 28 个对象及原目录身份，B 的导入与计算仍成功；forget B 后 prune 删除 25 个 Conda 包、1 个普通 Python 包、1 个 Python 和 1 个运行目录，逐一核对实际目录及对象记录已移除。项目配置、锁文件保留；无网络请求及管理器调用。1 通过、0 失败、0 ignored，含编译共 304.87 秒；命令、原始输出、删除对象清单位于 `target/p8-closeout-20261104-item3-round1/`。未改生产回收实现。
- 第一轮第 4 项未按原扩展范围完成。第一轮未能证明程序执行扩展能新增支持至少三个真实包，扩展停止；第二轮用户已决定本轮放弃 post-link 调用受管 Python/包内程序，也不支持激活与 post-link 组合，现行范围见 P8.4。互相依赖的包联合安装推迟，第二轮第 2 步只改循环拒绝报错。
- 第 4 项已有执行层测试：初次 5 个用例中 3 通过、2 失败，失败发生在测试以符号链接临时路径创建受控目录时；仅修正两个测试目录路径并补充非零退出拒绝返回可导入结果的用例后，6 通过、0 失败、0 ignored。后次执行 35.58 秒，仍在原连续 600 秒窗口内（结束时共 398.71 秒）；两次原始输出均保留于 `target/p8-closeout-20261104-item4-existing/`。
- 监督进程 ENOENT 已定位并按用户授权实施最小修复，check 当时已审核通过此修复（仅对应第一轮的监督进程清理问题）：普通脚本非零退出时，先等待现有 `session.close()` 成功交还清理责任，再报告脚本失败；关闭错误继续向上传递，不改监督器结构。此前异步清理与测试 TempDir 删除父目录发生先后顺序冲突，已单独复现；正常安装的持久缓存父目录不随脚本失败返回销毁，未发现普通用户操作复现同一错误的证据。修复前对照输出保留在 `target/p8-closeout-20261104-supervisor-diagnosis/`。临时对照测试已删除，清理完成断言合入永久非零退出回归测试；临时配置/进程快照及空目录已清除，只保留项目要求的原始命令、输出和结果。修复后主程序构建成功，6 通过、0 失败、0 ignored，整组 56.00 秒，输出未出现该监督进程错误，证据 `target/p8-closeout-20261104-supervisor-fix/`。该次修复不包含取消、超时或通信错误；后续限定范围的验证见第二轮第 1 步。
- 第一轮程序执行扩展的 300–500 行生产代码估算已随该范围放弃而作废；第二轮正式代码明显超过约 200 行须先停下说明。罕见并发防护新增 0 行。固定超时 60 秒已获同意，不开放配置；超时报错须说明包和脚本、未提交状态、进程是否已确认停止，以及查看脚本输出、修正包版本或依赖后重新执行原命令的操作。固定超时和对应报错已在第二轮第 1 步实施并通过实际 60 秒验证。
- 实际包适用性检查：基于当前上游源码，临时目录下生成的 GTK 输入法缓存会记录程序/模块绝对路径；gdk-pixbuf 的可迁移构建虽允许相对模块条目，`LoaderDir` 注释仍记录目录路径。当前对临时路径的字节检查也拒绝注释，所以仅开放执行不足以完成这些包的正常脚本安装。Jupyter 扩展还会向配置写入模板所在目录；PyOpenGL 的已查看脚本使用 Bash 专有语法；保留的 gsettings-desktop-schemas 43.0 包已带编译结果且没有 post-link，不能计作新增收益。没有把源码推断当成真实安装通过。这些扩展未实施；第二轮第 1 步缩减后的脚本范围及第 2 步循环报错已审核通过。

### 四个历史失败的定向处理（2026-11-01，已完成）

四个历史失败测试分轮分别定向通过，不是一次全量通过。两处代码修改均在测试文件内，未修改生产实现、未放宽检查：

1. 重复安装测试（`f2_exact_install_reuses_and_records_only_real_projects`）：仅在该测试内允许首次写锁并补齐 PythonSources 输入，新增有效锁及再次安装后锁内容不变的断言；保留原有的项目登记检查；定向通过。
2. 新项目复用测试（`f3_new_project_reuses_ready_layout_and_missing_never_commits`）：提供正确 Python 归档（本机已核验 SHA256 的 3.13.16 stripped 归档）后通过，该测试未修改。
3. 构建产物复用测试（`p3_nonce_installed_output_survives_unrelated_update_without_zip_cache_or_hooks`）：原始报错为符号链接锁目标拒绝；改用真实项目路径（现有 canonicalize）后通过，未修改生产检查。
4. 拒绝错误复用测试（`p3_strict_cached_source_outputs_reject_changed_backend_and_request_without_publication`）：提供正确 Python 归档后通过，该测试未修改。

后两个测试各 1 通过、0 失败、0 ignored（同一连续窗口 221 秒）。证据：`target/remaining-four-tests-20261101/`、`target/remaining-two-tests-20261101/`。前轮失败后未经审查自行重开窗口的纪律违反事实及各项更正保留在原证据（`13-corrections.md` 等）中，不改写历史。

限制：当时只完成定向验证；当前完整分组结果见第 4 步。不代表所有符号链接项目路径均已验证；原另外五个失败随无消费者实现删除，不是修复后通过；P8/P9 未整体完成。

### 安装固定锁输入（2026-11-01，已完成）

实施用户决定：实际安装使用本次初始锁数据——完成历史中断事务恢复后，把项目锁输入固定为该次观察（含观察到缺失的情况），此后磁盘上的改写、替换、删除重建或缺失后出现不再使安装拒绝，也不被采纳为新依赖依据。`BoundPythonLock` 增加固定来源构造（`bind_fixed`/`bind_fixed_conda`，仍持有真实会话与父目录 guard，`recheck` 按来源分派：Acquired 保持磁盘映像比较，Fixed 只做绑定/截止检查）；`ObservedProjectInputs::with_fixed_project_lock` 只在安装执行入口固定一次；锁写入新增 `replace_journaled_install`（写前备份字节/身份/mode 同一次观察取得，取消内容一致前置、写中比较与写后再读，结果取自本会话自写对象）；观察侧新增只读 `resolved_target`，固定输入记录初始真实目标，符号链接改指不改变输入、同步保护与跨项目归属检查。写前恢复备份与依赖选择输入分离；普通/locked 安装不新增写回。实测（`target/fixed-lock-install-20261101/` 日志 18–20）：修正后的恢复用例 1 过；定向组 116 过 / 0 败 / 1 ignored，其中 `fixed_lock_install_` 24 个全过；主程序构建 EXIT=0。指导会话已核对最终源码与证据（14 文件摘要与两份未跟踪快照一致）。产品边界规则已并入主架构文件。当时未运行完整分组，四个历史失败后来分别定向处理；当前结果见第 4 步。真实项目、跨平台及初始缺失锁的全部别名组合仍未覆盖；`acquire_conda` 的测试消费者告警未处理。

### resolved_build 完整入口删除（2026-11-01，已完成）

删除没有生产消费者的 resolved_build 完整准备与锁生成入口、专属连接方法（`build_with`、`complete_resolved_build`）、专属投影（`closed/projection.rs`、`check_group_shape`）、准备工具 `RegistryBuildFixture` 及 15 个专属测试（12 个未标 ignore，3 个删除前标 ignore）。生产仍使用的 `ResolvedBuildError` 迁移至 `closed.rs`，仅保留 Deadline/Revoked/SourceReceipt 三变体，原有报错文字与触发条件不变。当前生产锁生成（由 LanguageLease 推导）、数据上限数值及读取一致性检查未改变。编译通过；定向测试 98 通过 / 0 失败 / 0 ignored；指导会话已核对当前源码及证据。原三个相关失败测试随无用实现删除，不是修复后通过。证据：`target/resolved-build-removal-20261101/`。当时未运行完整分组，另外四项失败后来分别处理；当前结果见第 4 步。Build/Inventory 历史失败底层原因未确定。第二层模块与新增无用告警当时未审查，该范围已于同日下一批清理完成（见下节）。用户已决定取消"安装期间检查锁文件外部修改"的行为——该决定当时未随本批实施，已于同日"安装固定锁输入"批次实施完成（见该节）。

### 下层构建簇清理（2026-11-01，已完成）

删除七个无生产消费者的模块（build、seal、runtime_bootstrap、runtime_source、runtime_startup、runtime_snapshot、runtime_layout）及专属文件，共 21 个文件、9890 行；`runtime_target.rs` 缩编为纯 `freeze_observed_target` 投影。删除 140 个旧测试声明（其中 23 个原标 ignore），新增两个纯数据测试保留当前转换函数的 markers/平台标签/extras 规范化与 compatibility 拒绝检查。保留 `freeze_observed_target`（生产消费者：语言库发布与源码 Python 构建）、证据中的真实来源对象持有、现行 `recheck`、混合安装与锁模型功能；`closed.rs` 证据瘦身（10 个纯数据字段与 10 个无用方法），`recheck` 与移交行为不变。编译通过，定向测试 102 通过 / 0 失败 / 0 ignored；指导会话已核对源码及证据。证据：`target/cluster-cleanup-20261101/`。当时未运行完整分组，另外四项失败后来分别处理，当前结果见第 4 步；安装期间锁文件外部修改检查的取消当时尚未实施，后由同日"安装固定锁输入"批次完成（见该节）。不把本次删除写成 Build/Inventory 历史故障已修复，也不宣称全仓库所有无用代码均已清理。

### InstallRollback 删除（2026-10-31，已完成）

删除没有生产消费者的 InstallRollback 恢复实现及其专属撤销记录代码（含 `install_layout/publication.rs`、`finish_tracked`、`write_backend_meta_tracked`）和 35 个专属测试。原两个失败测试（`relative_install_paths_preserve_the_raw_entry_during_recovery`、`raw_install_symlinks_including_dangling_links_are_restored_without_following`）随无用实现删除，不是修复后通过。现行安装、发布（`finish`/`link`/`write_backend_meta` 等）与共享库 mutation 检查保留。编译通过；定向验证 85 通过 / 0 失败 / 1 ignored；属性修正后单测 1 通过。指导会话已核对最终源码摘要与差异。证据：`target/install-rollback-removal-20261031/`（`final-evidence-20261031.md` 及 01–07 日志）。当时未跑完整分组；其余失败的后续处理见上述记录，当前完整结果见第 4 步。

### 当前继续点（2026-11-09）

第 1、2 步及第 4 步均已审核通过；第 3 步除最后回收外的原断言已走过，最后回收依用户决定留到删除简化后验证。当前代码完整分组为 4987 通过、0 失败、87 ignored，覆盖 5074/5074，P4 另行通过。第 5 步文件归属及提交方案已获 check 通过，第 6 步按批准方案执行本地提交。

本次只整理 PLAN 和既有改动的提交分组，未修改已测试代码；删除简化与第 7–12 项均未开始。中间提交的依赖和限制见第 5、6 步说明。

| 项 | 状态 |
| --- | --- |
| P8 纯 Conda 完整生命周期 | 通过。`target/p8-real-conda-run7-20261029-result.json`，485.12 秒，0 失败，manager 调用 0。 |
| P8 混合 PyPI（mixed） | 第一轮独立冲突、失败保持和混装回收已通过；旧完整测试已使用完好 SciPy 和原始锁，已走到实际冲突与失败保持；最后回收在 600 秒截止时未结束，完整测试仍未通过，见本轮第 3 步。 |
| P8 脚本/依赖环/SCC | 第二轮约定的单包单对 `.sh`、fish/csh 提示、固定 60 秒和失败清理已通过；循环拒绝已显示包名。多包环境脚本和 GDAL 新语法不做，联合安装与恢复推迟。 |
| P9 | 基本未开始。`target/p9-macos-arm64-311-run2-20261029-result.json` 仅跑 5.63 秒即失败，导出/共享/冷暖恢复/拒删/forget/回收全部未验。 |

**SciPy 坏包已定性**：`scipy-1.14.1-cp313-cp313-macosx_12_0_arm64.whl` 内部 WHEEL Tag 写的是 `macosx_12_3_arm64`，且自身 RECORD 中 WHEEL 一行的摘要与大小都对不上，是**上游损坏的制品**；`macosx_14_0_arm64.whl` 逐条 RECORD 校验完好。产品拒绝坏包是正确的。用户已决定：**遇到坏包如实报错即可，不承担修复上游损坏的责任**，不为坏候选自动切换。

**P9 run2 失败为夹具缺口**：显式 stripped 与 schema7 冷启动无 Simple 两项均已通过；失败点是测试代理未提供 `https://api.github.com/.../attestations/...` 路由，代理按设计拒绝了未知请求。

### 待完成

1. 完成本轮第 5、6 步的提交审核和本地提交。第 3 步最后回收已按用户决定延后，不再拆分或重跑完整用例。
2. 下一轮单独简化删除并重做 P4 删除验证，实测回收用时；多包环境脚本、GDAL 新语法和依赖环联合安装另行决定。
3. P9 全部平台与真实项目验收（macOS arm64/x86_64、Linux glibc x86_64/arm64、musl x86_64/arm64）。
4. P9.1 查询与报错、P9.4 文档与工程收尾。
5. 清理：服务器 `/tmp/pakro-p8p9-20261029.9s8q6qb_`、`/tmp/pakro-inputs-20261028.tn43o9ph`；本机 Docker 容器与镜像（清单见 `target/p9-docker-environments-20261029.json`，Docker 已恢复停止）；本机 5 个 `pakro-p8-mixed-*`/`pakro-p9-pbs-*` 夹具待按 owner 清理。

### P6 收尾（已完成，仅留证据入口）

定向删除验证 9 父例通过（`target/reclaim-validation-results-20261028.json`）；Mac/Linux 最终日志已补齐（`target/p6-closeout-mac-source-20261028-results.json`）；腾讯云旧 P6 目录已删并核验（`target/p6-closeout-20261028/old-root-cleanup-result.json`）。

## 一、目标、已完成基础与最终范围

### 当前基础

P1、P2 保持“已完成”的状态，不重新开展原阶段。后续工作复用已有的：

- 精确 Python 安装、目录选用和共享内容。
- wheel、限定纯 Python 源码包的安装与构建。
- 项目关联、查询和指定对象删除。
- 统一 `pakro.lock`、严格恢复。
- 锁文件与项目记录的受控提交、中断恢复。

修改前和修复后完整分组结果见第 4 步；当前源码已完成本机 macOS arm64 分组复验。历史 P1/P2 完成结论不扩大为其他平台或完整真实项目通过。

### 本轮已确定的产品范围

| 项目 | 最终决定 |
|---|---|
| 解释器 | 只做通常使用的 CPython；PyPy退出本次产品范围。 |
| 用户依赖配置 | 只读取 Pakro 配置体系，不读取用户项目的 `pyproject.toml`、`requirements.txt` 作为依赖来源。 |
| 配置修改 | 开发者手动修改，Pakro不代写。 |
| 第三方包说明 | 必须继续读取包自身的依赖和构建说明，包括源码归档内部的 `pyproject.toml`。 |
| 更新锁 | 显式更新；优先保留仍满足新要求的旧版本和旧制品。 |
| 编译工具 | 用户准备编译器、SDK和系统开发库；Pakro检查缺项、固定实际输入并组织构建。 |
| Rust扩展 | 允许用户安装的Cargo作为受控离线构建工具，不允许它自行联网、更新锁或获取未准备的依赖。 |
| Linux布局 | 优先共享和写时复制；必要时允许复制位置相关包文件，保持独立文件所有权。同一组合跨项目复用，不复制整套Python。 |
| Conda资源 | Pakro直接读取公开包源、解析和安装资源，可以取得配套CPython；不安装或调用Conda、Mamba、Micromamba工具。 |
| Conda包源 | 默认仅使用conda-forge；用户可以明确配置其他公开源及顺序。 |
| Conda包脚本 | 受控支持安装脚本和环境设置脚本，禁止联网、修改其他项目及共享原件。 |
| 已取消 | 原P5、P7；项目下载、Git/local安装、editable、自动配置编辑均不恢复。 |
| 后续生态 | 本计划只完成Python，不启动Java、Node。 |

“Python完成”指上述明确范围形成完整、经过验证的使用流程，不承诺任意历史版本、任意构建后端、任意第三方脚本都能运行。不支持的输入必须在适当阶段明确报错。

## 二、统一使用规则与接口

### 1. 用户操作入口

下列新增入口属于本计划，实施前不能在文档中当作已有功能。

| 入口 | 行为 |
|---|---|
| `pakro install` | 无锁时生成首次精确结果；已有锁时严格遵守原结果。 |
| `PAKRO_LOCKED=1 pakro install` | 必须有完整匹配的锁；不重新求解、不改锁。 |
| `pakro install --update-lock` | 按当前项目配置准备新结果，成功后共同更新锁和项目使用记录。 |
| `pakro install --update-lock --dry-run` | 只展示直接声明变化和可确认的本地状态，不下载、不构建、不写入；未求解部分明确标注。 |
| `pakro deps install python` | 与普通项目安装使用同一生产内核，不另建安装逻辑。 |
| `pakro project forget <项目ID>` | 解除一个项目的登记和关联，不修改项目文件、不卸载库内容。 |
| `pakro project forget <项目ID> --dry-run` | 展示登记路径和将解除的关联，不写入。 |
| `pakro uninstall …` | 保留精确对象删除，不递归扩大目标。 |
| `pakro prune --dry-run` | 只读预览整个受管库中可回收及受保护的内容。 |
| `pakro prune` | 显式回收确定无人需要的受管内容。 |
| `pakro prune --resume <操作ID>` | 续行原删除操作，不新增候选；同时承接指定卸载产生的未完成删除。 |

`--update-lock`只操作当前项目的声明，不接受临时包追加或另一个解释器目标；与严格锁定模式、字节码恢复入口互斥。版本变化必须先由开发者写进配置。

项目目录移动采用“新目录成功登记，再forget旧登记”。不增加自动搬家识别、后台扫描或永久忽略名单。

### 2. 配置保持一套

继续使用现有`[tools]`和`[packages."python@完整版本"]`，不新增用户需要创建、命名、激活的环境对象。

- 原包版本字符串写法继续有效，默认表示PyPI包。
- 包声明增加表形式，允许`version`、`source`、`extras`、`marker`；Conda包另允许`channel`和`build`。
- `version`必须精确。`extras`开启该包的可选功能；`marker`决定该条声明是否适用于当前解释器目标。
- 不引入新的依赖组或命名环境；这些声明仍进入同一个项目图。
- Python声明允许明确选择`prebuilt`、`source`、`conda`来源；原字符串写法保持现有预编译来源含义。
- Conda公开源列表放在`settings.python.conda.channels`，默认仅`["conda-forge"]`。
- 编译工具可在`settings.python.build.tools`中明确指定路径；未指定时从当前系统发现一次并固定。SDK/sysroot允许显式指定，显式配置优先。
- 未知字段、非法组合、直接版本范围、来源混淆和归一化后冲突的包名均明确报错。

Conda和PyPI分别使用自己的名称、版本和依赖规则，不能把Conda的build字符串塞进PyPI版本规则。

### 3. 持久事实继续共用

继续保持四种事实：

- 配置：开发者意图。
- `pakro.lock`：精确语言、来源、依赖、构建输入和结果。
- catalog：本机项目选择与引用权威。
- manifest：实际内容和所有权。

保留P2统一锁的读取能力；扩展字段需要明确格式版本。旧边车锁不迁移、不恢复。普通查询不补写、不自动恢复。

新增对象或构建方式必须同时接入锁、查询、引用和删除，不能只完成下载。

## 三、实施阶段

实施顺序为：**接续检查 → P3 → P4 → P6 → P8 → P9**。原P5、P7保留“已取消”标记，不重新分配其含义。

### 接续检查：确认删剪后的工程可以继续开发

**实施状态：已完成。** 删剪后必要编译和定向回归已完成；P1/P2历史基础保留，不重开原阶段。原始证据由`target/p2-manual-*20261025*`等对应记录承载，后续变更按受影响路径单独验证。

### P3：配置读取、显式更新锁和项目使用记录

**实施状态：已完成。** 既定macOS arm64范围已通过，包含手工声明、显式更新锁、兼容旧结果复用、空依赖提交、精确ID forget与目录移动。原extras闭包、首次观察保留、源码产物复用、等价解释器写法及暖缓存锁核验问题均已修复。

完成证据：`target/p3-designated-final-approval-20261026.json`、`target/p3-cache-final-results-20261026.json`。缓存补验95.25秒/1父例通过，原失败组和另外两旧父例通过分开记录，不改写为一次全量通过。此完成范围不扩大为全部平台或ISAT验收。

**用户得到的结果：** 自己改完配置后，可以明确更新安装结果；不用的项目可以解除登记；其他项目不受影响。

#### P3.1 统一项目声明来源

- 退出用户项目`pyproject.toml`的依赖、`requires-python`及锁指纹观察。
- 修改一个不再读取的项目文件，不得影响Pakro选择或造成锁不匹配。
- 保留第三方包内部的依赖、构建和兼容性说明读取。
- 接入Pakro包声明的精确版本、extras和marker。
- marker依据实际选择的CPython目标判断；锁保留完整声明意图，安装图只包含当前生效的根。
- 不修改开发者配置，也不把不支持的声明当成空依赖。

#### P3.2 增加明确的锁更新意图

在现有安装内核区分“保持已有锁”和“按声明更新”，不能通过删除旧锁、伪造无锁会话或失败后自动重试求解实现。

更新流程：

1. 同次观察当前配置、旧锁和项目选择。
2. 根据新声明求解；旧锁中的兼容版本、来源和制品作为优先候选。
3. 旧结果是偏好，不是不可改变的硬约束；确有冲突才调整。
4. Python声明未变时，优先保留原精确解释器发行和build。
5. 准备并核验新内容，生成新锁文档。
6. 提交前重新核对配置、项目 revision、实际对象与来源；安装期间项目锁文件的外部内容变化按"安装固定锁输入"的产品边界处理（不重新选择依赖、不据此拒绝；需要写锁时写入本次准备结果，备份取自写前同一次观察）。
7. 沿P2提交机制共同更新锁与选择，报告实际变化。

准备失败不切换项目；已经准备好的完整共享对象可以留库，由显式回收处理。

配置变化后失效的旧Python目标图不得继续当作有效恢复结果；其他生态、合法未知字段和注释继续保留。

#### P3.3 处理空依赖与无锁情况

- 删除最后一个包、Python仍在时，提交“仅使用Python”的选择，不创建空布局。
- 完全不再使用项目时，通过forget解除关联，不根据配置缺失自动注销。
- 无锁沿用P2首次生成规则；严格锁定模式缺锁则拒绝。
- 不增加“曾经存在锁”的额外状态。
- 损坏或不支持格式的锁不能被当成普通版本变化覆盖。

#### P3.4 实现受控forget

- 按catalog中的精确ID定位，不要求原目录或锁文件仍存在。
- 取得项目保护后重读整个项目的选择状态。
- 存在未完成或损坏的提交记录时拒绝，不能绕过恢复。
- 清空所有当前选择，并在同一次catalog提交中移除项目登记。
- 保留递增版本记录，防止旧安装进程把已解除的关联写回。
- 不删除项目源码、配置、锁、实际包或解释器。
- 重复操作一个已经遗忘的登记返回“无变化”；之后重新选用项目可以重新登记。

#### P3.5 搬家与并发

- 新目录按当前配置和原锁正常安装或选用。
- 新关联成功后，用户再forget旧ID。
- 不根据项目名称、锁内容相同或依赖相同推断身份。
- 新父目录带来不同有效配置时正常报锁不匹配。
- 更新、forget和其他安装竞争时比较完整项目状态，不能只比较某一个目标槽。

**必须验收**

增包、删包、版本变化、解释器变化、最后一个包移除；兼容旧依赖保留、不兼容旧依赖调整；A更新不影响B；准备失败、并发更新、中断恢复；安装期间配置/revision/对象变化仍拒绝，项目锁文件的外部内容变化按"安装固定锁输入"边界处理；目录已删除仍能forget；旧进程不能复活已清空关联。

### P4：指定删除完善、显式回收与中断续行

**实施状态：已完成。** 既定macOS arm64隔离数据验收通过，包含共用删除内核、真实引用保护、无根组回收、持久进度、实际退出后的续行以及准确原因报告。

完成证据：`target/p4-final-verification-20261026.json`、`target/p4-final-source-20261026.json`、`target/p4-designated-final-approval-20261026.json`。30个库父例、7个CLI父例及6个受影响旧精确删除父例按对应源码分段通过；2个ignored为专属子进程helper。最新删除优化另在当前收尾中验证，不借用旧结果。

**用户得到的结果：** 能清楚看到什么还在用，并安全清理确定不用的内容；清理中断后可以继续。

#### P4.1 建立统一回收视图

- 从当前项目选择和未结束操作取得保护根。
- 从实际包、语言和布局记录取得依赖边。
- 不另建引用计数数据库。
- 历史安装记录、退休布局和无根对象之间的互相引用，不作为永久保护根。
- 指定卸载继续保护仍保留的上层包；prune从真实使用根判断整组可达性。
- 尚未关联项目的手动预装内容可以成为prune候选。

预览区分：可回收、被引用、正在使用、待恢复、损坏或未知，并展示原因。

#### P4.2 为删除增加持久进度

现有删除流程需要改为：

`核验并取得许可 → 保存删除记录 → 撤销可用状态 → 捕获原目录 → 逐项删除 → 更新元数据 → 完成记录`

删除记录至少固定对象身份、原清单、实际目录身份、捕获位置、相关布局和当前进度。

- 记录必须早于撤销可用状态。
- 恢复必须核对实际对象，不能凭目录名继续删除。
- 已删除、正在删除和从未开始的条目分别处理。
- 未知文件、目录替换、清单损坏不能通过递归删除“一并解决”。
- 未完成删除期间，同一对象身份不能重新发布成另一个实例。

指定卸载与prune共用这一套删除内核。

#### P4.3 批量回收和依赖环

- 将无根依赖环作为完整工作组处理。
- 按固定顺序取得对象保护，再重查引用；预览结果本身不授予删除权限。
- 取得不了完整工作组权限时释放已持保护并报告忙碌。
- 某对象暂时保留时，它仍需要的依赖也必须保留。
- 分批处理控制文件句柄和内存；超出明确容量的工作组报未完成，不拆开后误删。
- 恢复操作只处理原候选集合，不顺便加入新垃圾。

#### P4.4 清理布局和退休记录

- 删除包造成布局失效时，布局内容也进入受控回收。
- 清理成功后移除没有活跃选择的退休记录和派生索引。
- 保留并发失效需要的版本记录和稳定锁。
- 可丢弃下载缓存继续由缓存命令管理。
- 不扫描用户源码、构建目录或任意未知文件夹猜测垃圾。

本阶段恢复自己有完整记录的删除操作；不新增通用force、任意修复或自动覆盖损坏对象的能力。

**必须验收**

A/B共享、最后使用者释放、无根环与有根环、预览后新增引用、活跃租约、两个回收并发；在每个持久删除边界真实退出后续行；陌生文件、替换目录和损坏记录拒绝；结果准确区分已删、未删、忙碌和待恢复。

### P6：完成保留范围内的源码构建与Linux布局

**实施状态：已完成。** Mac arm64 与 Linux glibc x86_64 六个原生后端父例各 6/6 通过；源码 CPython 完整生命周期两平台均实际通过。其他平台验收留 P9。

- 后端：setuptools C、scikit-build-core/CMake/Ninja C++、meson-python/Meson/Ninja C、公开 MarkupSafe、maturin、setuptools-rust。
- 源码 CPython 默认配方用 `--disable-test-modules`，保留普通标准库、unittest/mock/doctest 及 SSL/hashlib/SQLite/ctypes/zlib/bz2/lzma 七项必要功能。
- 用户批准的查询/删除调整：本地查询 30 秒，显式删除/回收 300 秒；查询只核对 Ready 清单、物理绑定与解释器入口，安装/发布/物化/删除保留完整内容验证。
- round19：Mac Source 生命周期 309.499 秒、Linux 526.831 秒，均 1 父例通过、0 失败、0 ignored、一次删除完成未走 resume。
- 后续删除优化（复用父目录 fd、用已持有对象校验、移除重复哈希）晚于 P6 提交，已取得 9 定向父例通过并刷新主程序；Mac 完整 Source 补验 310.92 秒通过。
- 期望证据入口：`target/p6-mac-final-verification-matrix-v9-20261027.json`、`target/p6-tencent-round17-18-validation-results-20261027.json`、`target/p6-round19-delta-20261028.json`。各批只覆盖对应源码，不混称整树一次全绿。
- 已知限制：reflink 夹具用受管协议 runtime 加系统 Python，只证明布局；Mac 冷重建输出变化负例已实测拒绝，不能同时算作输出匹配的冷恢复正例。

**用户得到的结果：** 没有现成包时，Pakro能在明确支持的构建条件下自行组织构建，并继续共享、锁定和恢复结果。

#### P6.1 完成跨文件系统运行布局

- macOS继续使用现有写时复制。
- Linux支持可用的reflink；不支持时使用经过核验的独立文件复制。
- 复制仅用于运行布局需要的位置相关包文件，不复制整套Python。
- 同一组合由多个项目共用，不按项目生成完整副本。
- 实际采用的投影方式进入布局配方和清单，输入、输出都核验。
- 不使用可回写污染共享原件的硬链接。

#### P6.2 完成受控构建执行

复用现有构建协调器和沙箱，扩展受控子进程能力：

- 固定实际编译器、链接器、辅助工具、SDK/sysroot、环境和构建参数。
- 编译器等缺失时列出缺项，由用户安装；不调用系统包管理器。
- 构建过程禁止自行联网、修改项目和共享原件。
- 允许执行的程序必须有明确身份；不能把整个可写工作目录设为可执行白名单。
- 构建中新生成的程序必须经过受控登记后才能进入后续执行阶段。
- 超时、取消和父进程退出必须处理本次拥有的子进程及产物。

实施起点的macOS构建worker禁止派生子进程，因此这项能力必须有实际执行、监督和清理证据，不能只关闭`deny_process`就算完成。各批结果及现行状态见本节实施记录；不恢复已禁用的通用自动测试运行器。

#### P6.3 C/C++扩展

支持范围固定为：

- setuptools的PEP517及legacy构建。
- scikit-build-core对应的CMake/Ninja流程。
- meson-python对应的Meson/Ninja流程。

Python构建依赖由Pakro准备；编译器、SDK和系统开发库由用户准备。辅助工具若是明确声明的Python构建依赖可以随构建依赖取得，否则按缺项报告。

输出继续经过现有wheel检查、目标验证、共享发布及实际导入验证。未知构建工具、隐式下载和超出配方的操作必须拒绝。

#### P6.4 Rust扩展

支持maturin、setuptools-rust的受控构建路径：

- 用户提供Rust工具链。
- 源码必须包含可用的完整`Cargo.lock`；缺失或需要改变锁时明确报错。
- Pakro准备锁中固定的公开registry制品并校验摘要。
- 允许同一源码归档内部的workspace/path成员；不接Git依赖或归档外本地路径。
- Cargo仅在离线、锁不可变的条件下执行构建。
- 构建脚本和生成程序同样受执行边界约束。
- 不新增独立Rust产品生态。

#### P6.5 CPython解释器源码构建

- 从官方固定版本源码归档取得输入，不恢复pyenv。
- 使用版本化配方组织配置、编译和安装。
- 只做原生本机构建、普通release/GIL版本；不纳入交叉编译、debug、free-threaded或任意用户补丁。
- 不自动安装系统库，不默认安装pip或常用包。
- 结果发布为共享语言实例，来源、工具链、必要运行库和配方参与身份。
- 验证解释器、标准库和必要扩展，至少覆盖SSL、hashlib、SQLite、ctypes、zlib、bz2、lzma。
- 可选组件单独报告，不用“能打印版本号”代替解释器可用验收。

#### P6.6 归档、锁与恢复

- 源码包支持`.tar.gz`、`.zip`；解释器官方源码增加`.tar.xz`。
- 所有格式复用有界解包、路径和所有权核验。
- 原源码、固定构建输入、实际构建制品分别记录。
- 工具链、SDK、参数或配方改变不能错误命中旧结果。
- 暖恢复复用已核验产物；冷恢复使用固定输入，不能重新选依赖。
- 重建产物不一致时拒绝，不修改锁求成功。
- 本机路径修正属于明确布局配方；本机绝对路径不能成为可移植锁中的身份。
- 新构建对象同步接入查询、引用、指定删除和P4回收。

**必须验收**

真实C、C++、Rust扩展的编译、导入和函数调用；指定公开源码包；两项目共享；工具链变化拒绝错误复用；普通Linux文件系统；CPython源码构建与必要标准库；编译超时、中断、子进程清理；冷暖恢复和输出变化拒绝。

### P8：直接管理Conda资源及与PyPI组合使用

**实施状态（2026-11-09）：前两批及本机纯 Conda 生命周期通过，P8 整体仍未完成。** 平台包布局、混合 PyPI、显式 update-lock 和本轮限定脚本范围已完成相应验证；混装完整用例的最后回收按用户决定延后，依赖环联合安装与多包环境脚本另行决定。循环拒绝报错已显示包名，未把拒绝行为写成联合安装完成。

各批实际验证（按时间顺序，均保留原始结果）：

- 第一批：首次编译失败（两处 `PackageName` 类型错误）；修复后 81 通过 / 0 失败（声明 18、锁 45、请求 11、求解 7），主程序构建 exit 0。证据 `target/p8-batch1-repair-results-20261029.json`。同时关闭严格源优先、固定候选绕过 channel、锁缺项三个问题。
- 第二批：纯 Conda 最小路径接线；17 条编译诊断修复后库与主程序编译成功，短组 75 过 / 1 失败（测试漏传原 PBS 路径，补后通过）；CLI 解析测试修后 3 通过。
- 真实 Conda 安装 run1–run6 连续失败：run1–run2 下载连接被关闭；run3 在 metadata 阶段失败；run4 被 native 锁解析器拒绝 `install="conda"`；run5 在 idna 下载超时；run6 测试 HTTP 服务响应体提前 EOF。逐一修复的是**测试侧**问题（socket 未置阻塞、错误被吞、系统代理未隔离、native 锁 roundtrip fixture 写错路径），未提高任何额度、未关闭 TLS 校验、未加自动重试。
- 网络诊断排除项：官方制品 curl 单次 HTTP 200；TLS/代理 4 组合均正常；已确认真实缺陷是每包新建下载 client/连接池，已改为共用 transport。
- **run7 完整通过**：`target/p8-real-conda-run7-20261029-result.json`，485.12 秒 / exit 0 / 1 过 0 失败 0 ignored。公开 metadata 16 records 与原输入一致；initial/cold 各 16 次精确 HTTP 获取；warm 外网与镜像均 0 请求；cold 外网 0 且锁不变；A/B 同语言实例、idna 实际导入、声明失败保原事实、预览不变、引用拒删、forget/prune 全部完成；manager 调用 0、无残留进程。范围为 macOS arm64 纯 Conda。

第三批（混合 PyPI）现状（2026-11-09）：

- 真实输入：Conda numpy 2.3.3/py313h9771d21_0/osx-arm64（普通 GIL）、PyPI numpy-financial 1.0.0、SciPy 1.14.1（要求 `numpy>=1.23.5,<2.3`，构成真实冲突负例）。完整输入 `target/p8-complete-mixed-conda-inputs-20261029/inputs.json`，26 真实 records 加真实 osx-arm64 virtual 包。
- 已实现：mixed/conda_update、固定元数据与严格回放；不造假 wheel；恢复时在 registry 获取前拒绝重复 Conda 分发与同名 registry 替换；补齐 libblas/libcblas/liblapack 正常相对链接支持。
- 短例已过：A10 26 例、A11 32 真实例、batch5 库 27 例加 CLI 2 例，main 构建 0。
- 2026-10-29 的完整 mixed 父例 run1–run3 均失败（run3 为 415.77 秒），当时更新卡在损坏的 SciPy wheel；该历史失败记录保留。后续改用完好的 macOS 14 wheel，独立的真实冲突、失败保持和最终回收已于 11 月 4 日通过。第二轮完整用例使用保存原锁严格安装，首装、暖共享、冷恢复、真实 SciPy 冲突、锁与 catalog 保持、撤回声明、拒删及 forget A 后 B 正常运行均通过原断言；最后一次回收在 600 秒截止时未返回，按用户决定留到删除简化后验证，完整父用例不计通过。最新结果和原始输出位置见前文第 3、4 步记录。
- 限定 post-link、单包单对 `.sh` 激活/撤销、环境差分、fish/csh 提示、固定 60 秒及脚本失败/超时/SIGINT 路径均已验证。依赖环联合安装与恢复推迟，本轮只补循环拒绝中的包名。单个 Conda 包对应多个 Python 分发有映射与图投影的单元覆盖，未宣称独立完成真实多分发生命周期验证。
- run2–run7 测试 root 已按 owner 清理（`target/p8-fixture-cleanup-20261029.json`，删除 388,648,524B，无残留），原配置/锁/catalog 证据字节与 SHA 保全。

**用户得到的结果：** 无需安装Conda工具，也能使用Conda包源中的配套Python、原生库和Python包。

#### P8.1 来源与独立求解语义

- 复用已有Rattler、Conda元数据和求解能力，接入冻结操作上下文。
- 明确包源列表、顺序、目标平台和noarch；不隐式追加来源。
- 使用严格源优先级；显式指定来源的包不转投其他来源。
- 保留Conda的版本、build、依赖、约束和虚拟平台包语义。
- 直接声明必须精确；build不唯一时列出候选并报歧义。
- 未知平台不能退回noarch；glibc包不能直接用于musl目标。
- 不复用旧的“删除原prefix再重新安装”流程。

#### P8.2 制品、所有权与唯一解释器

- 支持`.conda`和`.tar.bz2`。
- 固定来源、实际制品摘要及清单，不按文件名猜缓存身份。
- Conda提供的CPython进入语言库，必要运行依赖进入共同引用图。
- 一个实际语言身份只有一个payload，不在每个项目或包库再展开一套。
- 包原始内容与前缀改写后的运行布局分开。
- 前缀改写针对最终稳定位置，处理noarch、入口脚本和原生库加载。
- 文件碰撞、大小写/Unicode碰撞、入口冲突在发布前拒绝。

#### P8.3 Conda与PyPI混合

采用固定方向，避免另造一个跨生态万能求解器：

1. 先求解并固定Conda图和解释器。
2. 从实际Conda制品的元数据识别它提供的Python分发。
3. 将可证明的分发作为固定候选交给PyPI求解器。
4. PyPI不得替换Conda已经提供的同一分发。
5. 无法证明对应关系或发生版本、文件、原生库冲突时明确报错。

不能仅凭两个包名字相似就认为它们相同，也不能静默安装两份同名分发。

#### P8.4 受控包脚本

- 第二轮现行范围：保留一个项目最多一个包的一对 POSIX `.sh` 激活/撤销脚本，不开放多包环境脚本，也不补 GDAL 四种新写法。环境脚本处理方式以后单独设计。
- 激活脚本与 post-link 同时出现继续拒绝。post-link 保持现有独立项目包、无其他包依赖、无 PyPI 混装的窄范围；本轮不允许调用受管 Python 和包内程序，未来是否在正式位置执行脚本另行决定。
- 拒绝 pre-link、卸载类脚本、不成对或缺少 `.sh`、激活与 post-link 组合、post-link 超出窄范围时，错误应包含包名、脚本名和原因。每个脚本执行任务固定最多 60 秒，不开放配置项、环境变量或测试缩时开关；用户取消和更早的整体截止时间优先。
- pre-link 和卸载脚本不在本轮范围，遇到必须执行这些脚本的包明确报告不支持及原因。
- 同时提供 `.sh` 和 fish/csh 的包使用 `.sh` 正常安装，并明确提示其他 shell 版本不执行；只有不支持格式、无法完成必要环境设置的包才拒绝。
- 安装脚本只能在本次拥有的私有准备布局中运行。
- 禁止联网、修改其他项目、共享原件或未授权宿主位置。
- 脚本新增和修改的文件进入所有权与结果核验。
- 激活脚本在准备阶段受控求取环境变化，保存为可应用的环境描述。
- 进入项目目录应用已有环境描述；离开时用现有环境差分恢复。
- 不在每次进入目录时重新执行任意包脚本。
- 需要用户交互、读取秘密状态、外部写入或无法表示的动态行为明确拒绝。
- 不允许脚本调用Conda/Mamba等管理器补装依赖。

本轮核验的 `libxml2-16 2.15.4 heb56d2d_1` 只有 `etc/conda/activate.d/libxml2-split_activate.sh`，没有撤销脚本。其原始内容如下；当前实现不支持这些语法，包含该包的项目会明确拒绝。

```sh
#!/bin/sh

if test -n "${XML_CATALOG_FILES:-}"; then
    xml_catalog_files_libxml2="${XML_CATALOG_FILES}"
    XML_CATALOG_FILES="${XML_CATALOG_FILES} "
else
    xml_catalog_files_libxml2=""
    XML_CATALOG_FILES=""
fi


# Replace space with '%20'; equivalent to
# conda_catalog_files=${CONDA_PREFIX// /%20}, except trailing space is
# ignored.
conda_catalog_files=""
ifs_libxml2="${IFS}"
IFS=" "
rem="${CONDA_PREFIX}"
for pre in ${rem}; do
    while test "${rem#"${pre}"}" = "${rem}"; do
	conda_catalog_files="${conda_catalog_files}%20"
	rem=${rem#" "}
    done
    conda_catalog_files="${conda_catalog_files}${pre}"
    rem=${rem#"${pre}"}
done
IFS="${ifs_libxml2}"

conda_catalog_files="file://${conda_catalog_files}/etc/xml/catalog file:///etc/xml/catalog"
export XML_CATALOG_FILES="${XML_CATALOG_FILES}${conda_catalog_files}"
unset conda_catalog_files ifs_libxml2 rem
```

#### P8.5 统一锁、恢复和回收

- 在统一锁和图中明确区分PyPI、Conda、语言和构建对象。
- Conda固定源、subdir、版本/build、摘要、完整依赖、平台要求和布局配方。
- 冷恢复按固定制品恢复，不重新取仓库信息求解；暖恢复不联网。
- 原始包摘要与本机路径改写结果分别核验。
- 所有发布、项目选择、查询和恢复共用现有catalog与提交机制。
- `conda-meta`如生成，只作为派生内容，不成为第二套权威。
- P4必须同时理解Conda包、必要原生库、混合布局与在途操作。

**必须验收**

纯Conda项目、noarch包、原生扩展、Conda与PyPI混合项目；无第二份Python；多个公开源与build歧义；错误摘要、平台不符和脚本越界拒绝；中断恢复；A/B隔离；指定删除与无根环回收。用管理器哨兵证明没有委托Conda、Mamba、pip或uv安装。

### P9：Python功能收尾与完整验收

**实施状态（2026-10-29）：基本未开始，P9 整体未通过。**

唯一的完整父例 `p9_pbs_wheel_single_platform_lifecycle` 只跑了 5.63 秒就失败（`target/p9-macos-arm64-311-run2-20261029-result.json`）：

- 已确认通过的两项：显式选择 `install_only_stripped` 发行、schema7 冷启动不读 Simple。
- 失败点：测试代理没有提供 `https://api.github.com/repos/astral-sh/python-build-standalone/attestations/sha256:<digest>?per_page=30` 这条路由，代理按设计拒绝了未知请求（拒绝型正向控制本身通过）。这是**夹具缺口，不是产品缺陷**。
- 因此后面全部未验：packaging 导入、A/B 同实例共享、暖恢复零请求、冷恢复固定制品、引用拒删、forget A 保留 B、forget B 后回收。

第一次尝试 run1 失败原因不同：未显式选 stripped 而落到默认 `install_only`，已修测试配置。

材料准备（未执行，不能计通过）：

- ISAT：61 索引页、81 制品（352MB）已核对原清单，`target/p9-isat-input-check-20261028.json`。旧验收脚本需去掉过时 binary/路径/私有代理，并补真正冷恢复与回收。
- PBS 归档：官方维护系列 3.11–3.15，六目标 30 候选记录在 `target/p9-pbs-assets-20261028.json`。Mac arm64、Mac x86_64、Linux glibc arm64、musl arm64 均已 5/5；Linux glibc x86_64 仍在准备。Rosetta 只能记翻译执行，不能算 Intel 原生。
- 已编译但 0 行为的主要平台：GNU ARM64（AArch64 ELF，96.108 秒）、Alpine musl ARM64（musl ELF，180.591 秒）。Mac x86_64 与 Linux x86_64 仅部分完成。**编译成功不等于平台验收通过。**
- 网络条件：系统代理开着，工具输入用代理、产品走默认路径会有差异；已按动态观测处理，未改系统配置。

全部工作结束后必须清理：服务器 `/tmp/pakro-p8p9-20261029.9s8q6qb_`、`/tmp/pakro-inputs-20261028.tn43o9ph`；本机两个 owner 容器及其精确镜像 tag（清单 `target/p9-docker-environments-20261029.json`），并恢复 Docker Desktop 为停止。

#### P9.1 查询和合理报错

- 所有保留的本地查询使用同一已提交事实。
- 展示精确版本、来源、build、实际实例和使用者。
- 增加已知包的远端版本查询：`ls-remote --package <名称> --source pypi|conda`；Conda允许指定已配置channel。
- 不承诺PyPI全库全文搜索，不通过搜索隐式安装插件。
- 错误区分配置无效、缺项、依赖冲突、不支持能力、内容损坏、正在使用和待恢复。
- 给出的下一步命令必须真实存在。
- doctor默认只读，不自动修复、补装或执行未知配置。

#### P9.2 支持矩阵

实施起点固定（2026-10-25）：按[Python官方维护状态](https://devguide.python.org/versions/)，稳定且仍维护的系列为CPython 3.11、3.12、3.13、3.14、3.15。3.10已结束维护，3.16属于开发系列。此项固定验收系列范围，不表示对应平台或制品已通过验证。

本轮接续验收固定输入为CPython **3.13.16+20261013**，`aarch64-apple-darwin`、`install_only_stripped`，归档SHA256为`16a5332fb0dcb77f163fc8f8cbccd95be1644cc53e68e5d2a79f50afd6680a0e`；本轮已只读核对原归档摘要相符。其余系列/平台实际验收的完整版本与制品摘要须在取得输入时继续明确记录，当前均未验收。

必须覆盖：

| 平台 | 必须验证的范围 |
|---|---|
| macOS arm64、x86_64 | 预编译CPython、wheel、支持的源码包、源码解释器、Conda适用制品 |
| Linux glibc x86_64、arm64 | 同上，并验证普通文件系统及可用reflink |
| Linux musl x86_64、arm64 | CPython及有实际兼容制品/配方的PyPI路径；不能冒用glibc Conda包 |
| 不纳入 | Windows、PyPy、交叉编译、debug/free-threaded、editable |

实施开始时将官方仍维护的稳定CPython系列、实际验收完整版本和制品摘要固定在PLAN中。使用已有验证归档作为基线，再补其他适用组合。没有上游兼容制品的组合明确列“不支持”，不能假装下载成功即支持。

#### P9.3 真实项目和资源验证

- ISAT使用用户已有源码及材料，验证完整依赖安装、选择、启动、关键导入和已有业务烟测。
- 补完整依赖图的重复使用、跨项目共享及冷恢复。
- 另设小型源码构建项目和Conda混合项目，证明ISAT没有覆盖的路径。
- 不负责下载项目、模型或修复应用算法；没有实际验证的模型推理不能写成通过。
- 保留固定大图资源测量，记录耗时、句柄峰值和释放后状态。
- 分批持有、复用同次观察和减少重复扫描；不削弱内容核验，不用提高额度掩盖泄漏，不擅自引入跨操作哈希缓存。

#### P9.4 工程与用户说明

- 退出PyPy产品入口和无消费者遗留，不删除仍被CPython实际使用的公共内核。
- CLI帮助、补全、Schema、README和实际行为一致。
- 工程继续直接使用Cargo/Shell，不恢复任务系统和旧自安装工具。
- 验收脚本摆脱本机绝对路径、特定测试binary哈希和私有代理。
- README提供安装、配置、使用、更新锁、forget、回收、恢复和错误处理的完整短指南。
- 不把发行网站、自更新基础设施或IDE专属接入加入本次Python完成门。

## 四、测试与“Python完成”的判断标准

### 每批变更需要的证据

| 类别 | 必须证明 |
|---|---|
| 正常使用 | 实际安装后能导入、执行；目录选择与查询一致。 |
| 共享 | A/B共用实际实例，选择隔离，改变A不破坏B。 |
| 锁 | 普通恢复不重解；显式更新保留兼容旧结果；来源和产物不被静默替换。 |
| 提交 | 准备失败不切换；并发与他方提交不互相覆盖（项目 revision 与会话/guard 保护）；项目锁文件的安装期间外部内容变化不触发重选或拒绝（见"安装固定锁输入"边界）；中断可恢复。 |
| 删除 | 仍被引用或使用时不删；无人使用的对象、布局及依赖环可显式回收。 |
| 构建 | 实际编译结果可运行，工具及输入明确，无隐藏依赖下载或管理器委托。 |
| Conda | 配套解释器唯一，混合冲突明确，脚本受控，冷暖恢复与回收闭合。 |
| 只读 | 查询、预览及被拒绝操作不写业务状态、不补装、不自动恢复。 |
| 资源 | 无泄漏、无限重试或绕过预算；失败状态如实输出。 |

### 验证纪律

- 只运行本批变更和受影响路径需要的测试，不每次重跑全部历史测试。
- 每个宣称完成的能力必须有实际行为证据；零测试、全ignored、只编译成功均不算功能通过。
- 本轮代码编写、长构建和验证由实施主会话亲自执行，check 只读审核；自动多进程运行器保持禁用。
- 同一批使用既定target、features和构建配置，遵守每组600秒规则。
- 日志放`target/`，记录命令、输入、源码对应关系、实际退出码、用例数和未覆盖项。
- 不能以增加无关测试数量替代关键失败场景。

本轮只执行用户指定的第二轮六步，按用户接续决定完成第 5、6 步。P9 的跨平台、真实项目和其他后续检查不在本轮范围；历史完成结论不扩大，缺少平台或输入结果时保持待验证。

## 五、实施Agent与审查Agent的交接规则

### 实施Agent

- 按依赖顺序实施上述工作，不恢复已取消功能。
- 复用现有生产内核，删除被替代且没有消费者的旧实现。
- 每完成一批，向审查会话提供完成内容、变更文件和仍需验证的问题；由实施主会话统一更新PLAN。
- 提交审查时提供对应代码差异、实际命令、完整日志位置和真实结果。
- 遇到无法满足合同的技术问题，应报告具体阻塞，不自行降低保护、偷偷换来源或扩大产品范围。
- 不新建HANDOFF、TESTS、FILES、开发日志或平行计划；不自动提交或推送Git。

### 审查会话负责验证与协调

- 本轮只读核对代码差异、实际调用关系与原始测试输出，不运行测试、不改写源码；需要补充验证时通过消息交给实施主会话执行。
- 重点检查范围是否扩大、旧路径是否回流、共享与引用是否正确、锁是否被静默改写，以及失败和恢复是否真实成立。
- 明确区分阻塞问题、普通建议和未覆盖证据。
- 修复交回实施Agent；审查依据发生变化后，只复核受影响部分。
- 完成结论必须绑定实际源码和验证结果，不以文档勾选、测试名称存在或旧阶段通过代替。

全过程继续停用dev-flow，不恢复其固定TDD循环或审查流程。
