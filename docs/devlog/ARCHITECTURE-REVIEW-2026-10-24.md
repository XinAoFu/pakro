# Pakro 架构阶段交叉盘点：当前实现与 R00–R45

日期：2026-10-24（Asia/Shanghai）。代码基线：`3d2a6c291`；此前F1–F4截止提交：`77d5969a8`。本轮仅盘点并更新项目规则，没有修改业务代码、运行产品构建/测试或提交Git。

**当前进度应描述为：R00–R06的已验收基础，加上F1–F4的Python实际使用主线，再加限定registry源码包构建与ISAT基础启动。** 后来的F/S覆盖了不少R07之后的任务内容，但没有逐项完成那些任务的完整合同，不能写成“只做到R06”，也不能写成“已经做到R31/R45”。

**本报告是架构决策前的事实基线，整个项目停用dev-flow skill。** 后续用户已确认P1–P9顺序及仅文档细化P1，当前决策和文件/验收范围见[主架构](../../Pakro主架构文件与Agent规则.md)及[PLAN前部](PLAN.md)。下方待讨论项不再作为未获方向确认的阻塞，代码仍未开始。历史日志及R详案不改写，`devlog/`只是保留的记录目录。

## 1. 阅读范围与判断方式

本次以Git跟踪文件为仓库文件全集，完整枚举并读取扫描4,052个文件：4,046个UTF-8文本文件，共922,059行；其余6个为二进制或非UTF-8资产。包括隐藏工程配置、源码、测试、注册表、vendor数据、发布脚本及历史release notes。文件总量约34.9MB。扫描基线是本轮修改文档之前的工作树，未把`.git/`、编译缓存、下载归档或仓库外的真实ISAT项目算作源码。

| 范围 | Git跟踪文件数 | 本次核对方式 |
| --- | ---: | --- |
| `src/` | 478 | 深读三库、programs、目录选择、查询/删除与配置接缝；其余模块按职责、符号、调用者扫描。 |
| `cli/` | 238 | 核对真实命令注册、分派、新旧安装/查询/变更入口及测试脚本；生成资源作范围核对。 |
| `runtimes/` | 1,134 | 深读共享Python和旧backend接缝，检查Java/Node/Conda资产；registry/vendor作全量文本扫描。 |
| `packages/` | 387 | 深读Python求解、wheel、sdist、worker、锁、旧环境与Node/Java消费者。 |
| `tests/` | 1,254 | 核对F/S注册、真实CLI断言及验收driver；其余测试清单/文本扫描，不从存在推断通过。 |
| `docs/`及根主架构 | 42 | 对照R详案、F/S计划、交接、资产/测试记录及全部本轮历史日志。 |
| `packaging/` | 448 | 发布/生成输入及历史记录扫描，确认仍有旧产品能力的工程依赖。 |
| 其他根文件/隐藏配置 | 71 | Cargo、锁、任务配置、CI、Codex hooks等范围及依赖核对。 |

这里的“全量扫描”不等于逐行人工审阅92万行，也不是全仓代码正确性认证。语义深读集中在本次R/F/S对照所需的生产链和证据；旧task/system/dotfiles/daemon的全部内部实现、每份生成资源和二进制内容未逐项验证。

判定分三层：**源码有实现 → 实际生产入口消费 → 有对应运行证据**。只存在模块、函数、测试名或历史勾选，均不自动跨越这三层。本轮额外只读抽查了现存`target/`中的F/S日志；没有把旧测试重复执行，也没有将历史结果当成本轮新测试。

## 2. 从旧计划到现在，实际多了什么

| 阶段 | 当时交付的能力 | 对旧R计划的主要覆盖 | 已知验收边界 |
| --- | --- | --- | --- |
| R00–R06 | 基线与runner、程序合同、真实配置观测、项目快照、共享身份、受控IO与锁、一份共享Python发布与租约 | R00–R06本身 | 历史分项验收，R06有150个不同用例及本机真实CPython共享证据；不等于现在全仓或全平台通过。 |
| F1 | 进入项目按精确声明选已有Python；缺项不隐式下载；必要hook/shim接入及持久项目选择 | R09、R13、R18、R26的必要部分 | 本机目录/命令查找路径；不包含IDE或外部进程全生命周期。 |
| F2 | 显式精确Python安装、发行build歧义拒绝、来源pin复核、共享复用、旧写删旁路拒绝 | R06扩展、R07、R10、R16的必要部分 | 官方PBS预编译CPython；不是解释器源码构建或全部backend改造。 |
| F3 | 真实registry依赖求解、wheel验证、包树/布局共享、脚本及导入、真实依赖边、同catalog关联提交 | R08–R16、R23的必要部分 | 单一公开registry、精确直接依赖；deps install已接通，不代表deps add或Locked已完成。 |
| F4 | 已知项目/语言/包及关联查询；唯一指定对象删除；引用链拒删、实际排他保护与身份复核 | R18、R21、R25、R26的必要部分 | 指定删除，不含批量prune、声明移除或完整回收恢复矩阵。 |
| S1–S3 | registry纯Python `.tar.gz` 源码包；PEP517/legacy；初始/动态构建依赖、metadata、真实worker、原源码与输出双证据 | R30/R31的限定部分、R32的legacy部分 | 本机无编译器子进程后端；不委托pip/uv。不是任意sdist格式或全来源源码支持。 |
| S4及现场修复 | 真实ISAT完整依赖选择与基础启动；来源兼容、回溯、COW原生邻接、FD和选择耗时修正 | 跨R10–R13/R30–R31的真实集成验证 | 用户确认24项直接依赖、60包关联后的基础启动；自动完整冷组及完整业务仍未通过。 |

当前实际路径可以概括为两条：

```text
进入目录 / env查询
  → 观测项目配置与已有锁信息
  → 读取已发布语言/包及项目选择
  → 已齐全：返回路径；目录hook另行提交必要关联后应用
  → 缺项：提示，不下载

明确 install / deps install
  → 精确版本与来源约束
  → 获取或复用共享Python
  → registry求解；需要时执行受控纯Python源码构建
  → wheel核验、共享包树及布局
  → 同一catalog提交项目选择/依赖图
  → 目录选择使用；查询/指定删除读取同一权威
```

这里尚没有配置文件、可移植项目锁和catalog三者的通用联合提交。

## 3. R00–R45逐项交叉表

旧状态来自[2026-10-18存档](archive/PLAN-R00-R45-2026-10-18.md)：R00–R06完成；R20明确后置；R36/R37撤下；其余当时未实施或未开始。“部分覆盖”表示当前已有可用功能或内核，但不足以将原R任务整体勾选。证据编号对应第4节，不按任务数量计算百分比。

| 任务 | 原计划目标 | 当前实现与仍缺范围 | 判定 / 证据 |
| --- | --- | --- | --- |
| R00 | 建立构建/测试基线，固定runner | 历史基线与runner修复已完成；不代表今天全仓重新通过。 | 历史完成；E01 |
| R01 | 三类程序、独立命令意图、冻结上下文与报告 | 合同已实现；实际F入口未统一消费所有request/context/apply门面，不能推导use/upgrade/add都已接线。 | 历史完成，生产接线有边界；E02 |
| R02 | 真实native配置观测 | 读取同一held bytes并复核身份，供后续使用。 | 历史完成；E02 |
| R03 | 完整项目快照、读写分离、拟议编辑 | F1/F3复用配置发现和快照；拟议编辑不是已接通的声明事务。 | 历史完成；E02 |
| R04 | 内容共享身份与项目选择分离 | 共享键不含ProjectId；后续增加仅语言和真实包图选择，保持同catalog权威。 | 历史完成并扩展；E03 |
| R05 | 受控IO、读写打开、锁顺序 | 新三库/F/S复用原IO核；后续增加同一保护内的批次核验和FD处理。 | 历史完成并扩展；E03 |
| R06 | 只发布一份完整语言payload | 共享语言已实现，F2真实CLI复用；不含Linux/PyPy完整矩阵或全部语言生态。 | 历史完成并扩展；E03 |
| R07 | 封住全部旧破坏旁路 | 有统一mutation路径拒绝核，Repair/Purge/Uninstall/Rollback等有生产接点；P1细查发现Force/PostInstall枚举仅见测试直接调用，不能称全部入口已封。4-pyc恢复只是精确特例。 | 部分覆盖；E04、E15 |
| R08 | 原始制品、包树、构建产物复用 | F3/S有实际包树和布局发布；新`publish_blob`门面目前仅测试消费，不能称完整blob/tree/build层全部生产化。 | 部分覆盖；E05 |
| R09 | 稳定项目身份、位置、持久共享引用 | F1登记，F3真实根/节点/依赖边/pin，F4反查已接；位置变更及完整声明变更矩阵未验收。 | 部分覆盖；E03、E06 |
| R10 | 下载来源、凭证与本地复用 | 官方PBS和单一公开Simple、代理/期限/pin已接；多私有源、认证组合及全来源矩阵未完成。 | 部分覆盖；E07 |
| R11 | 普通求解与严格Locked回放 | Resolve与源码continuation已接；共享包安装明确拒绝Locked，底层locked_graph不等于产品可用。 | 部分覆盖，Locked未接；E07、E08 |
| R12 | wheel验证及真正共享布局 | 五scheme、包树、入口、`.pth`、原生扩展已接；跨wheel原生邻接COW仅有macOS实现。 | 部分覆盖；E05、E09 |
| R13 | 共享Python与项目包适配 | 新F链持同一LanguageLease，不复制完整运行时；旧runtime_snapshot及其消费者仍存在，尚未全仓替换。 | 新主链交付，旧链未退出；E09 |
| R14 | 可移植精确锁与真实对象 | catalog已保存精确图和源码/输出事实，已知runtime pin会复核；新链不写项目锁，完整统一锁与冷回放未交付。 | 部分内核，产品合同未完成；E08 |
| R15 | 配置、锁、关联联合事务与恢复 | 当前是单catalog CAS，失败保留旧选择；没有通用多文件WAL。专属pyc journal或旧环境journal不能替代。 | 部分覆盖；E06、E08 |
| R16 | install完整共享wheel流程 | 精确Python与项目包安装已接生产CLI，S增加限定sdist；原任务中的完整Locked/事务仍缺。 | 受支持主线交付；E10 |
| R17 | use成功准备后再改声明 | 仍主要走旧Toolset/Config，未完成新共享链的声明/锁/选择事务。 | 未完成新接线；E11 |
| R18 | 九个本地查询 | env/current/ls/installs/project等已有共享分支；where/which/bin-paths仍走旧Toolset，未全部统一。 | 部分覆盖；E12 |
| R19 | 五个发现/版本比较查询 | 旧命令仍可提供历史能力；新query_program下统一discovery/comparison未完成。 | 未完成新接线；E12 |
| R20 | IDE/外部解释器及子进程生命周期 | DirectoryPython仅返回路径描述，不携带运行租约。 | 保持后置；E12 |
| R21 | 删除候选、可达性、许可与恢复 | F4有EX、最新引用复查、撤Ready和指定回收；未完成全库可达GC与完整中断恢复。 | 部分覆盖；E13 |
| R22 | upgrade切换新选择、保留他人旧对象 | 手动改配置后的新选择不等于upgrade事务；命令仍走旧路径。 | 未完成新接线；E11 |
| R23 | deps install与deps add原生路径 | deps install已接F3/S；deps add仍执行provider外部命令。 | 一半主线已接，add未接；E10、E11 |
| R24 | unuse/deps remove更新声明和剩余图 | deps remove仍委托provider；unuse仍旧配置编辑并默认旧prune，不是新catalog关联变更。 | 未完成新接线；E11 |
| R25 | uninstall与显式prune | 指定语言/包uninstall已接；新共享库批量prune尚未完成。 | 部分覆盖；E13 |
| R26 | 十四查询及目录驱动已有内容选择 | 目录驱动选择已交付；查询只完成部分共享接线。 | 部分覆盖；E12 |
| R27 | Direct URL精确来源 | 新共享根只接受plain exact registry pins；registry下载URL不能算Direct URL用户来源已支持。 | 未完成新接线；E07、E14 |
| R28 | Git精确快照 | typed continuation等底层资产存在，新共享安装消费者未接。 | 未完成新接线；E14 |
| R29 | local source与项目自身依赖 | 实际ISAT源码副本用于测试，不是本地包来源能力；新链未接。 | 未完成新接线；E14 |
| R30 | 构建依赖闭合与动态metadata | S已实现registry纯Python范围的初始/动态要求、递归/环/预算、metadata及同solver续传；其他来源与编译工具尚缺。 | 限定范围交付；E14 |
| R31 | 固定输入worker与产物验证 | S已实现实际worker、隔离、原源码和输出双证据；仅registry `.tar.gz`，禁止编译器子进程，完整冷恢复未接。 | 限定范围交付；E14 |
| R32 | editable与legacy独立生命周期 | legacy fallback已支持；PEP660 editable活源码、重建及卸载生命周期未实现。 | legacy局部交付；E14 |
| R33 | 语言源码构建及默认包旁路退出 | 新共享路径只支持预编译Python；包sdist不是解释器源码。旧python-build/default pip分支仍存在。 | 未完成；E04、E16 |
| R34 | Conda接入三库 | 原Rattler/Resolvo后端存在，但仍是旧prefix模型；共享语言所有权/统一图/混合冲突未接新链。 | 旧能力保留，新架构未完成；E16 |
| R35 | Node多实例/peer/SCC/共享引用 | 有旧npm解析和安装；按eco:name唯一的旧图、hardlink/refs等不能代表新多实例图。既定顺序仍在Java之后。 | 旧能力保留，新架构未完成；E16 |
| R36 | Go产品生态 | 旧计划已撤下，本轮没有恢复；旧代码存在不改变范围。 | 已撤下，不计完成；旧计划R36 |
| R37 | Rust独立产品生态 | 旧计划已撤下；Pakro用Rust开发/Python扩展需Rust均不等于恢复此任务。 | 已撤下，不计完成；旧计划R37 |
| R38 | Java/JDK与原生Maven依赖管理 | Java运行时资产保留，包依赖仍有`mvn dependency:go-offline`委托，无新Maven三库图。 | 旧能力保留，新架构未完成；E16 |
| R39 | 清理旧Pakro测试数据 | 清target/自有临时树或修复pyc不是该任务整体清理；没有已完成证据。 | 未完成；旧计划R39、PLAN后移表 |
| R40 | 退役重复与不合规管线 | 新Python局部改接，旧复制/Store/provider等消费者仍在。 | 局部推进，整体未完成；E09、E16、E17 |
| R41 | 工程测试/生成器脱离产品任务命令 | 新增F/S直接脚本，但README和`.config/tasks.toml`仍依赖pakro run/exec/render链。 | 局部推进，整体未完成；E17 |
| R42 | 删除非核心模块前先拆核心依赖 | 必要hook、sandbox等仍交叉使用；S还同步了旧exec/run/task配置，尚未全面拆分。 | 未完成；E17 |
| R43 | 改接辅助命令并退役排除入口 | 部分辅助查询已接；install-into、tasks、daemon、dotfiles、plugins、OCI等仍注册/分派。设计取消不等于已删代码。 | 部分改接，退役未完成；E17 |
| R44 | 依赖、构建输入、Schema/生成资源清理 | hook.py发行清单已补；整体Cargo/Schema/命令树清理未完成。 | 局部修正，整体未完成；E17、E01 |
| R45 | 完整Python→Java→Node与故障/平台验收 | F/S及ISAT基础启动有效，但不是完整Python阶段、Java/Node或最终产品验收。 | 未完成；E01、第5节 |

## 4. 源码与记录证据索引

路径均相对仓库，行号对应本次源码基线；文档更新后的行号以标题/内容重新定位。

- **E01 历史验收：** [旧R计划](archive/PLAN-R00-R45-2026-10-18.md)的R00–R06完成段；[F1–F4接续日志](log/2026-10-20-1-F1至F4接续.md)原84–89行；[ISAT截止日志](log/2026-10-24-4-ISAT验收截止与提交.md)。
- **E02 合同与观测：** [install/request.rs](../../src/core/programs/install/request.rs)的`normalize_intent`；[project/snapshot.rs](../../src/core/libraries/project/snapshot.rs)的`observe_project`；[native_python.rs](../../src/core/config/config_file/native_python.rs)；[install/apply.rs](../../src/core/programs/install/apply.rs)的`install_exact_python`目前只有测试调用，真实CLI另走Python门面。
- **E03 三库基础：** [language/identity.rs](../../src/core/libraries/language/identity.rs):103、[install/runtime.rs](../../src/core/programs/install/runtime.rs):61、[project/repository.rs](../../src/core/libraries/project/repository.rs):34、[read_views.rs](../../src/core/libraries/project/read_views.rs):205，以及[pakro-store/control](../../packages/crates/pakro-store/src/control/mod.rs)。
- **E04 旁路保护：** [language/mutation.rs](../../src/core/libraries/language/mutation.rs):12/27列出mutation类型；[install_rollback.rs](../../runtimes/src/toolinstall/install_rollback.rs)、[system_install.rs](../../runtimes/src/toolinstall/system_install.rs)有生产接缝。P1细查补充：Force/PostInstall目前只见测试直接调用，路径拒绝核也不能代替旧命令首次副作用前的意图准入。枚举存在不等于消费者齐全或旧代码已退役。
- **E05 包树与布局：** [package/tree.rs](../../src/core/libraries/package/tree.rs):164/193与[layouts.rs](../../src/core/libraries/package/layouts.rs):70/78/228是真实消费链；[blob.rs](../../src/core/libraries/package/blob.rs):9的`publish_blob`调用仅见测试，`prepare_tree`门面也不能按存在当完成。
- **E06 单catalog提交：** [shared/commit.rs](../../src/core/programs/shared/commit.rs):1明确范围，108起为选择提交，304起为包选择提交；不是配置/锁/catalog通用联合WAL。[project/package_graph.rs](../../src/core/libraries/project/package_graph.rs)提供真实图视图。
- **E07 当前安装限制：** [programs/python.rs](../../src/core/programs/python.rs):67拒绝Locked包安装，137起仅plain exact registry根，304/432限定public registry，530–535固定PyPI来源；218附近说明不写声明或项目锁。[Python plugin](../../runtimes/src/plugins/core/python.rs):587起接精确共享安装。
- **E08 有内核但未完成接线：** [locked_graph.rs](../../packages/src/python_resolution/source/locked_graph.rs)、[lock_session/document.rs](../../packages/src/python_resolution/lock_session/document.rs)、[environment/journal.rs](../../packages/src/python_environment/journal.rs)已有回放/文档/日志资产；不能覆盖E07的生产拒绝或E06的单文档范围。
- **E09 新旧环境并存：** [shared_runtime.rs](../../packages/src/python_environment/shared_runtime.rs):15持共享租约；[build.rs](../../packages/src/python_environment/build.rs):121仍持`OwnedRuntimeSnapshot`，旧[runtime_snapshot.rs](../../packages/src/python_environment/runtime_snapshot.rs)仍有消费者；[prefix/clone.rs](../../packages/src/python_environment/prefix/clone.rs):60非macOS拒绝原生namespace COW，100为macOS实现。
- **E10 已接真实入口：** [install.rs](../../cli/src/commands/install.rs):231/384、[deps/install.rs](../../cli/src/commands/deps/install.rs):55；[真实wheel CLI测试](../../tests/e2e/packages/test_native_install_shared_wheel)及[不委托manager测试](../../tests/e2e/packages/test_native_deps_no_manager)。
- **E11 未迁移变更入口：** [use.rs](../../cli/src/commands/use.rs)、[upgrade.rs](../../cli/src/commands/upgrade.rs)仍旧Toolset/Config；[deps/add.rs](../../cli/src/commands/deps/add.rs):72与[deps/remove.rs](../../cli/src/commands/deps/remove.rs):65执行provider命令；[unuse.rs](../../cli/src/commands/unuse.rs):130/146仍默认旧prune。
- **E12 查询与目录：** [directory.rs](../../src/core/programs/directory.rs):32/49；[env.rs](../../cli/src/commands/env.rs):89、[hook_env.rs](../../cli/src/commands/hook_env.rs):135、[installs/ls.rs](../../cli/src/commands/installs/ls.rs):52、[project.rs](../../cli/src/commands/project.rs):195/215。where/which/bin_paths仍旧Toolset；旧兼容目录可能引用共享payload，但F2 exact不建这种兼容目录，不能说已统一投影新项目layout，也不能说这些旧命令完全不可用。
- **E13 指定删除：** [uninstall/select.rs](../../src/core/programs/uninstall/select.rs):9、[reachability.rs](../../src/core/programs/uninstall/reachability.rs):102、[language/removal.rs](../../src/core/libraries/language/removal.rs):176、[package/removal.rs](../../src/core/libraries/package/removal.rs):133；CLI共享删除入口在[uninstall.rs](../../cli/src/commands/uninstall.rs):223。
- **E14 源码构建：** [install/build.rs](../../src/core/programs/install/build.rs):158/257/306/366/442分别涉及registry准备、源码session、初始/动态依赖及续传，454拒绝非registry来源；[archive.rs](../../packages/src/python_source_build/archive.rs):444限`.tar.gz`、544提供legacy fallback；[worker.rs](../../packages/src/python_source_build/worker.rs):125/203禁止网络和编译器子进程；[hook.py](../../packages/src/python_source_build/hook.py)未实现PEP660 editable hooks。
- **E15 现场增量：** [programs/library.rs](../../src/core/programs/library.rs):35限定4个pyc恢复；[main.rs](../../src/main.rs):73及[process_limits.rs](../../src/crates/pakro-util/src/process_limits.rs)只调本进程soft FD；[directory.rs](../../src/core/programs/directory.rs):29冻结300秒选择预算；[环境选择修复日志](log/2026-10-24-3-ISAT环境选择.md)记载32.71秒真实只读结果。
- **E16 其他生态及旧链：** [Python plugin](../../runtimes/src/plugins/core/python.rs):630限预编译，1664/2269附近保留旧python-build/default pip；[Conda backend](../../runtimes/src/backend/conda.rs):209/374/478仍旧prefix；[system.rs](../../packages/src/system.rs):85/97委托mvn；[npm.rs](../../packages/crates/pakro-index/src/npm.rs)、[resolver/graph.rs](../../packages/crates/pakro-resolver/src/graph.rs)、[materialize_node.rs](../../packages/src/materialize_node.rs)仍是旧Node图/布局。
- **E17 退役和工程依赖：** [commands/mod.rs](../../cli/src/commands/mod.rs):322/487仍注册并分派排除项；[构建入口](../../src/build/main.rs):33保留通知等构建资产；[任务配置](../../.config/tasks.toml)和[README](../../README.md)仍使用产品run/exec生成与测试。[S测试runner](../../tests/scripts/s-source-check.sh)可直接运行，但真实driver仍绑定本机输入及测试binary。

## 5. 哪些结果已证实，哪些没有

已有记录确认F1–F4的64项不同核心用例、8条CLI验收及真实公开PyPI安装；官方Python完整下载/运行/双项目共享/重装复用也有历史通过记录。这些是前轮结果，本轮没有重新跑全套。

本轮只读抽查的日志与文档结论一致：

| 现存日志 | 抽查事实 | 能证明的范围 |
| --- | --- | --- |
| `target/f-core-final-20261020.log` | 44 passed / 0 failed | 某个核心子组；不能单凭此文件声称64项全部。 |
| `target/s-source-final-unit-green-20261023.log` | 源码相关多个子组通过，含实际worker/coordinator | 源码限定范围的核心行为；不是完整ISAT冷组。 |
| `target/s4-advisory-map-core-green-20261024.log`及app日志 | resolver72、candidate20、registry55通过，另有显式ignored | 求解/策略/来源回归，不能把ignored计入通过。 |
| `target/s4-package-selection-green-main-20261024.log` | 包库44通过/1辅助ignored、directory3通过、main构建成功 | 相关包库/目录行为与当时构建。 |
| `target/s4-directory-env-selection-green-20261024.log` | 真实只读环境选择32.71秒，exit0 | 选中实际共享layout；不单独证明GUI或模型业务。 |
| `target/s4-isat-full-functional-batch-20261024.log` | 明确`TIMEOUT 600 seconds`且停止自有进程 | 该完整自动组未完成，绝不能改记通过。 |

用户在前轮最后明确确认ISAT在Pakro环境基础启动成功。对应实际项目为24项直接依赖、revision3、关联60包和Python `3.11.17+20261013`。因此，**实际安装、选择、基础启动已经验收**；以下仍单列未覆盖：

- 自动完整冷安装验收、完整ISAT图重复/跨项目零获取；小型源码core的跨项目通过不能替代整图。
- NMS、标注格式导出、模型推理等完整业务断言；断言写在driver里不表示已经执行通过。
- Linux/PyPy/其他架构完整矩阵；原生邻接COW目前还存在明确平台限制。
- 编译器源码后端、URL/Git/local/editable/Conda以及Java/Node新架构。
- 外部进程完整生命周期保护、通用repair/force、批量回收及全窗口恢复。

约33秒的完整目录选择和每包较多FD仍是后续架构/性能问题。现有修复通过增加本进程soft FD容量和减少同一保护范围内的重复核验使当前案例可用，不代表常数FD算法或即时目录选择已经完成。

## 6. 新架构阶段需要面对的差异

以下记录决策前提出的问题；现已由主架构及PLAN的P1–P9承接，获准的是方案和文档细化，尚无代码实施授权。

| 需要决定的边界 | 当前事实 | 架构讨论要回答什么 |
| --- | --- | --- |
| 统一程序入口 | R01合同门面与真实F生产链并不完全重合 | 哪些合同继续作为唯一入口，哪些只是重复抽象；以实际消费者决定，不能仅按旧文件表补壳。 |
| 项目选择与可移植锁 | 当前catalog足以支撑使用与指定删除，Locked安装仍拒绝 | 哪些操作必须改声明/锁，如何引入严格回放和多文件恢复，是否分开交付。 |
| 完整项目生命周期 | install/deps install可用，add/remove/use/upgrade仍旧路径 | 如何使声明变更、最后一个根移除、引用释放及旧选择保留形成完整使用闭环。 |
| 包库存储层次 | 真实包树/layout已用，新blob门面尚未生产消费 | 原始归档、展开树、构建输入/输出、投影各层如何统一复用与回收，避免重复权威。 |
| 性能与保护 | 大图内容核验慢、FD持有较多 | 在保持身份/引用/完整性约束下，怎样减少重复工作与句柄；不把扩大预算当最终结构。 |
| 旧命令退役 | 架构已取消若干能力，代码/Schema/工程流程仍保留 | 先拆哪些工程依赖，替代验收达到什么条件后才删除；install-into取消如何真正落地。 |
| Python完成标准 | 当前是可用主线与真实应用基础启动 | 完整锁、源码/来源/平台/恢复需要达到什么程度，才满足既定Python→Java→Node顺序。 |

## 7. 本轮规则与文档修正

已更新主规则、README、docs入口、PLAN、HANDOFF、FILES和TESTS的当前说明，纠正“只到H1-d1”“F阶段尚未开始”“F测试不存在”“源码构建全部后移”等与现状不符的入口描述；历史过程段落明确为历史，不改写当时失败、暂停、授权或通过记录。

整个项目不再加载或强制执行dev-flow skill，不保留它的固定阶段门、TDD循环、双轴审查或目录格式作为强制流程。用户独立确定的架构决定权、真实证据、600秒执行上限与数据保护继续有效；用户全局skill安装不在本次修改范围。

新架构应以本报告的实际接线和证据作为起点。旧R详案保留完整目标索引，F/S计划保留已交付范围与过程，两者都不能单独替代当前实现盘点。
