# Pakro 首版逐文件实施计划

日期：2026-10-18（Asia/Shanghai）。

**状态：R00–R06 已完成；首版计划现为F1–F4，用户明确将库内容/关联查询及指定删除纳入首版，并重申完整精确版本要求。2026-10-18用户下令开始F1：行为测试与可编译桩已写入并注册，编译与预期失败由用户手动确认；F2–F4与最小实现尚未开始。** 长期功能没有被永久取消；本次未运行编译/测试/fmt/check，未提交Git。后续编译、测试和检查仍由用户手动执行。

架构仍以[主文件](../../Pakro主架构文件与Agent规则.md)为准。原R00–R45详案完整保存在[历史详案](archive/PLAN-R00-R45-2026-10-18.md)，用于追溯；它不再是必须逐项完成的前置队列，也不恢复历史测试授权。计划更新不代表获得开工许可。

**本次细化方式：** 恢复原详案的逐文件清单、接口交接、实施顺序、边界和验收合同，仍只有F1–F4四个交付阶段。下文行号为2026-10-18工作树的定位锚点，实施前按函数名重新定位；包含已有未提交/未跟踪实现，不以HEAD替代当前文件。

## 为什么缩减

旧计划的问题主要是交付顺序，而不是所有检查都无用：

- 普通install被R07/R10–R15的完整横向任务挡住；用户很晚才能用到已完成的共享内核。
- 目录选择被放进完整查询接线，实际最先需要的只是配置匹配、已有Python路径和缺项提示。
- 首次安装和选用，本不必等待完整升级、物理回收、多文件恢复以及所有源码来源。
- 已有文件/制品/锁内核应直接复用，不在每个消费者重新建一套防护或重复跑未改变的全套测试。

新的实施单位是“用户能完成一件事”，而不是“又完成一个内部模块”。四个阶段分别交付选择、安装、包使用、查询与指定删除；不把原详案全部工作藏进几个标题，首版之后的能力仍明确后移。

## 第一版边界

- 先验收用户当前的macOS arm64机器和官方PBS预编译CPython；Linux/PyPy完整矩阵放到后续验证，不声称首版已经覆盖。
- 用户沿用已有项目配置，不自动编辑声明；项目成功选用的语言/包及依赖关联必须有可复核的持久记录，供F4查询与删除检查。纯查询不登记项目，内部关联维护不增加用户操作步骤。
- 包阶段先一个公开Simple来源、已有兼容wheel的普通Python包，优先用纯Python包验证；真实传递依赖和兼容检查不能跳过。
- 目录内已有就选用；缺项由用户决定安装；安装没网明确报错；没有独立离线模式。
- 允许必要内部路径/启动适配，不要求用户创建、命名或逐项目激活环境；不委托pip/uv完成本应原生完成的安装。
- F4开放明确指定的语言/包删除：有项目或依赖引用就列出并拒绝，确认无引用才可删除。自动批量prune、强制覆盖、原位修复及自定义postinstall仍后移；旧无许可删写旁路须在副作用前拒绝。
- 原有其他功能不因此被删除；只说明这一条新共享路径的首版支持范围。Python→Java→Node的长期顺序和既定最终能力仍保留。

## 贯穿F1–F4：完整精确版本与真实关联

- 用户下载请求及项目直接清单必须写完整精确发行版本。Python例如`3.11.14`；`3.11`、`3`、`3.11.xx`、通配、范围、`latest`/浮动别名均不得自动补全。`xx`只是用户举例的占位，实际填写时须是真实补丁号。
- 安装命令可读取配置中已明确的完整版本，但参数和配置都没有完整版本就报错；拒绝发生在远端获取/下载及业务写入之前。包按其生态的真实完整发行号表达，不随意补零造版本；查询可列候选，但不会自动把候选变成下载目标。
- 包作者metadata中的传递依赖范围仍要真实解析；每项实际下载前必须确定精确版本/制品并记录，不要求用户手填全部间接依赖。完整版本不替代平台、发行build、来源和摘要校验；同版本多实例有歧义时列出候选，不任取或选最新。
- F1–F3逐步维护项目→精确语言/包、包→实际依赖及适配→语言的关联。退出目录只撤当前终端选择，不释放项目持久引用；新选择成功后才更新关联，失败、配置损坏或项目暂不可访问不把已有引用当作零。
- 完整可移植锁与复杂事务矩阵可以后移；实际版本和最低关联记录、其真实写入保护不能后移，否则F4不得开放删除。关联更新与查询职责分开，不让只读查询隐式写库。

## 文件落位、读写权限与执行纪律

所有路径相对仓库根目录；`{a,b}.rs`表示明确列举的两个文件，不是任意目录授权。每阶段分别列现有修改、新增和默认只复用文件。未列文件若成为真实必要接缝，应先把具体路径、调用理由和影响补入本计划及日志；不借“相关文件”扩大范围，不以机械照旧详案为由创建所有模块。

| 能力/文件落位 | 首次交付及后续消费者 | 能读写什么 |
| --- | --- | --- |
| `programs/shared/exact_version.rs`（完整路径见F1） | F1建立，F2/F3/F4复用 | 只解释已持有输入；不自行获取候选、修正声明或产生IO。 |
| `programs/query/{snapshot,paths}.rs` | F1目录选择/环境输出，F4扩库清单与关联 | 只读真实配置/已发布对象；查询为空不初始化库，损坏不伪装空。 |
| `programs/install/selection.rs`与`shared/commit.rs` | F1维护仅语言选择，F2/F3复用，F4读取/受控变更同一权威 | 仅受管内部catalog的登记/关联；不写项目配置/项目锁，不顺带下载。 |
| `libraries/project/{repository,selections,read_views}.rs` | F1持久事实，F3加入精确包图，F4反查 | 同一CatalogModel/codec/CAS；`projects.json`、路径列表、refs计数均不能另作权威。 |
| `libraries/language/`既有R06内核 | F1只读、F2获取/发布、F4指定删除 | 由实际租约/发布/删除持有者控制；不能凭目录或描述性JSON修改内容。 |
| `libraries/package/`的F3文件 | F3制品/共享布局，F4列举及指定删除 | 正式包内容、真实manifest、必要轻量适配；不写共享语言全局site-packages。 |
| `programs/uninstall/`及两库`removal.rs` | F4 | 仅指定无引用对象及其删除状态；无强制/递归/批量GC权限，不改项目声明。 |

首版坚持一个权威catalog完成内部登记/选择提交。准备阶段可先发布未被项目选中的完整共享对象；失败时允许留下明确无项目关联的完整对象，不能谎报项目成功。对象就绪后才提交选择，旧选择保持到提交成功；已完成并可安全结束的临时/退休保护要收尾，不把历史记录变成永久引用。若确实要写项目配置、项目锁或第二个权威文件，则已超出本计划的简化写域，应收窄分支或补该项真实提交保护，不把完整R15默默塞回所有阶段。

- 每阶段按“行为测试并注册→用户执行确认失败→最小实现→用户复测→Spec/Standards两轴静态审查”推进；只有有实际结果才勾选，失败原因和未执行范围单列。
- 新文件须在表内父mod注册。CLI在独立binary crate，公开最小程序入口/请求/报告；库IO/owner保持私有。删除默认是**无**，只有对应旧调用确已替代且用户验收后才局部退役。
- 本轮只把计划写具体。后续每次只执行用户授权的阶段，不因文件清单完整而自动开工，不自行运行编译/测试/fmt/check，不自动提交或清理target/用户数据。

## F1：进目录就选中库里已有的 Python

**状态：** ☑ 测试已写并注册（2026-10-18，含可编译桩）；☑ 用户确认预期失败（2026-10-19，单测 4 项均以 "f1 ... is not implemented" 形态失败；e2e 暂存通过后在 `command -v python` 选择断言处失败；格式通过；证据 `target/f1-manual-20261019-{104029,104202,104520,105020,105327,105541}.log`）；☑ 最小实现（2026-10-19，2026-10-19用户授权Agent自行执行测试≤10分钟/组；`target/f1-manual-20261019-121806.log` 单测4/4+e2e+格式全部通过；相邻回归 model37/catalog13/prefix-control90/selections11/journal14/language37 与 env/activate e2e 通过；`test_activate_export` 经 git stash 对照确认为改动前既有失败，不属F1）；☑ 用户验收（2026-10-19 通过：用户在真实终端按演示命令确认四项行为——A/B 同一 payload/bin/python、缺项仅提示不下载、退出撤回；暂存/重启语义由 `target/f1-manual-20261019-121806.log` 与用户复跑的暂存输出证明）；☑ 两轴静态审查（2026-10-19 通过：Spec 轴 7 PASS/1 PARTIAL、Standards 轴 5 PASS/2 PARTIAL/0 FAIL；唯一阻塞"提交锁序与规格相反"及两条高价值建议已修复并复验全绿 `target/f1-manual-20261019-200450.log`；非阻塞建议记 F2+ 跟进，见[2026-10-19日志](log/2026-10-19-1-F1-RED确认与夹具修复.md) 20:10 条目）。**F1 五步全部完成。**本次注册：下表四个行为测试与e2e、`tests/scripts/f1-test-cases.txt`与`tests/scripts/f-manual-check.sh`（新增`--list`模式）；`programs/install/selection/tests.rs`含`#[ignore]`环境驱动暂存助手`f1_e2e_stage_shared_language`（读`PAKRO_F1_STAGE_DATA_ROOT`/`PAKRO_F1_STAGE_VERSION`，仅供e2e脚本调用，不在`f1_`过滤运行中执行）。

**先完成：** R00–R06已有基础。先交付F1，不等待F2–F4。

**用户看到什么：** 在已启用的原终端接入下进入A/B项目，按配置选中库内已有Python；缺少时只提示，离开或切换目录恢复正确选择。

**修改现有文件：**

| 文件与定位锚点 | 本阶段只改什么 |
| --- | --- |
| `src/core/programs/shared/context.rs:53`、`src/core/programs/install/context.rs:38` | shared/context只冻结显式输入与权限，不读配置、不反向依赖query/install；有效声明归并由install/context的共享纯投影及query/snapshot消费同次观测完成。分开只读与关联写入权限，不借旧Config加载登记、联网或执行配置脚本。 |
| `src/core/libraries/project/snapshot.rs:131` | 复用`observe_project`、`declarations`、`effective_packages`、`recheck`，补只读语言选择需要的投影；不复制配置parser，不开放声明写入。 |
| `src/core/libraries/language/repository.rs:94`、`manifest.rs:38`、`lease.rs:143` | 补已发布实例的本地列举、精确筛选和重新取得真实租约的桥。当前`acquire_existing`需要完整安装计划，不是按版本查询接口；须从真实manifest恢复/核验所需身份，不能联网重造计划。 |
| `src/core/libraries/language/repository/tests.rs`（2026-10-18接缝） | 仅参数化既有夹具助手：新增`archive_triple`/`install_spec_for`/`archive_for`（目标/构建/变体），原`install_spec`/`archive`委托不变；服务F1歧义用例与e2e离线暂存，不新建第二套夹具。 |
| `src/core/libraries/project/selections.rs:82`、`packages/src/python_environment/model.rs:146` | 同一模型支持显式仅语言选择、项目身份/位置/声明观测；保留原共享布局分支、revision/nonce语义。禁止伪造空graph/receipt或`g-*`布局填满旧结构。 |
| `packages/src/python_environment/model/codec.rs`及`codec/{state,encode,decode,read}.rs` | 在原有界codec内表示新增事实，关联和引用同一快照；缺字段/未知格式明确诊断，不另造第二个序列化实现或自动迁移旧测试库。 |
| `packages/src/python_environment/catalog.rs:12`、`prefix/control.rs:12` | 原读取核改为可消费R05只读control，写入仍复用同一CAS；不能为查询先open_writer。旧消费者委托同一核，不复制catalog。 |
| `cli/src/commands/hook_env.rs:51` | 新共享Python分支调用只读选择，再调用受控关联应用，成功后输出路径；保留目录检测/旧路径撤回，避开daemon、任务和自定义enter hooks。快速返回不能跳过必要关联状态核对。 |
| `cli/src/commands/env.rs:63` | 改用只读选择结果，保留shell/JSON/dotenv输出；退出该分支的`install_missing_versions`、lazy shim准备和隐式登记。 |
| `runtimes/src/toolset/{builder,toolset_env,toolset_paths}.rs` | 只提取/改接已确定选择的环境组合接缝；不再经旧resolve补全版本，不以offline字段代替精确版本检查，不被旧uv/venv/exec_env覆盖。 |
| `cli/src/commands/hook_not_found.rs:26`、`runtimes/src/shims/mod.rs:100` | 新共享Python路径的command-not-found、lazy/wrapper只提示缺项，不能触发安装；保留尚未迁移生态的独立代码。 |
| `src/core/programs/{mod,shared/mod,install/mod}.rs`、`src/core/libraries/project/mod.rs` | 注册下列新增模块及测试，只向CLI公开应用请求/结果/入口，不公开库writer或租约构造器。 |

**拟新增文件（当前不存在，不是已完成接口）：**

| 文件 | 单一职责 |
| --- | --- |
| `src/core/programs/shared/exact_version.rs` | 纯输入门：复用已有生态版本解析，拒绝不完整用户声明；Python完整版本语法复用`runtimes/src/plugins/core/python/native.rs:150`，必要时只提取该原函数的可见接缝。 |
| `src/core/programs/query/{mod,snapshot,paths}.rs` | 同次只读上下文、本地候选匹配、可执行/bin路径及缺项/歧义报告；不登记、不创建库、不联网。 |
| `src/core/programs/install/selection.rs` | 目录选择的受控应用动作；只登记已存在的精确对象，不调用下载/安装流水线。 |
| `src/core/libraries/project/{identity,location,repository,read_views}.rs` | 稳定项目身份、本机位置绑定、同一catalog的持久IO和只读投影；F4继续扩这些文件，不另建登记库。 |
| `src/core/programs/shared/commit.rs` | 项目登记与选择的单catalog提交；复用原锁/CAS/原子替换，首版不创建通用多文件WAL子系统。 |

**复用而默认不改：** `src/core/environment/hook_env.rs:701`的环境撤回、`src/crates/pakro-util/src/env_diff.rs:76`的EnvDiff/patch算法；R05的`packages/crates/pakro-store/src/control/{read,write,lease,ownership}.rs`；R06发布/inventory核。不得复制一套PATH管理或文件锁实现。

**接口交接（目标合同，不代表Rust函数已存在）：**

- `open_query_context(context, scope) -> QueryContext`：消费冻结OperationContext和R03观测，持只读库句柄。
- `select_existing(context) -> SelectionReport`：返回精确候选身份/路径，或Missing、Ambiguous、InvalidDeclaration、Corrupt；结果只是描述，不授予写入。
- `apply_directory_selection(context, observed, selection) -> OperationReport`：消费同次观测与候选；按项目锁→对象SH→catalog顺序重取保护，复核声明/Ready，登记和选择一次CAS成功后才允许hook输出新路径。不能拿query的对象锁反向抢项目锁。
- 仅语言记录含ProjectId、位置绑定、声明观测、group/target、LanguageKey、真实Ready发布事实及revision/transaction；明确没有包布局。F3在同一选择模型增加真实包组合，不能用伪布局凑合同。

**实施动作（按以下顺序，完成后更新上方状态）：**

- 在现有`programs`门面加入最小只读选择/路径接口；只做目录选择用到的能力，不先写齐十四个查询。
- 补“完整精确声明→本地已发布语言实例”的匹配桥，复用R03配置观测与R06只读库/租约；不接受模糊版本，不先访问远端来补成最新，实例有歧义就明确报告。
- 成功选用时维护最小项目身份/位置、声明观测及精确语言关联；这是独立的受控应用动作，不是只读查询的隐藏副作用。登记内部完成，不要求用户额外创建环境。
- 改接`cli/src/commands/hook_env.rs`、`env.rs`和必要的原toolset/env_diff接缝；只复用目录检测、配置选择和环境差异，不把旧daemon/user task执行搬入新选择链。
- 该用户路径中的自动建venv、补装、lazy/command-not-found隐式安装先切断；若仍可达旧postinstall或清理，也必须在F1断开，不能等F2。F1只接读取、选择及其必要关联提交，遵守D07/D08/D09。

**复用的原任务：** R18/R26的最小部分、R03/R06已有能力。没有包声明时，不需要先完成R08包库、R12/R13包适配、R14全量锁或R15联合事务。包声明在F3接通前明确报告尚未支持，不能假装已经加载。

**交付验证：** A/B选同一精确Python时实际解释器和标准库来自同一payload；切换到不同声明/缺项项目不会静默用错版本或联网下载；退出项目撤回本次路径选择。由用户运行少量真实场景，不以打印预设JSON代替。

**测试文件与必需断言（拟新增，实施时注册）：**

| 测试文件 | 合同 |
| --- | --- |
| `src/core/programs/shared/exact_version/tests.rs` | `f1_incomplete_version_has_no_effects`：完整版本通过；3.11/xx/范围/latest拒绝，网络调用与业务写入为零。 |
| `src/core/programs/query/paths/tests.rs` | `f1_local_selection_reports_missing_and_ambiguity`：一个精确候选可选；缺项/多个实例分别报告；坏manifest不能算未安装；查询不登记。 |
| `src/core/programs/install/selection/tests.rs` | `f1_language_only_selection_survives_restart`、`f1_failed_selection_keeps_old_references`：真实重开catalog，语言引用存在且无假布局；配置变化/提交失败不丢旧引用。 |
| `tests/e2e/cli/test_f1_directory_selection` | 真正启用原终端接入，进入A/B/缺项项目再退出；解释器/标准库来自同一payload，PATH撤回正确，缺项不下载，不生成完整venv。脚本先用`#[ignore]`暂存助手离线发布夹具语言（同版本主机目标归档，无网络、无安装命令），再走真实activate/hook链；真实PBS归档留给用户手动场景。 |

**阶段边界/退役条件：** 不下载、不处理包布局、不编辑配置/项目锁，不接所有查询或外部进程生命周期。`env`查询不写关联；只有目录选择应用动作写关联。仅退役已被新Python分支替代的旧调用；不整删activate/deactivate/hook_env/toolset。若本分支能触达旧清理/安装，须当场切断，不能借F2后置放行。

## F2：用户决定安装缺少的 Python，装好就能选用

**状态：** ☑ 测试已写并注册（2026-10-19，含可编译桩；RED 已由 Agent 自测确认——4 项 f2_ 单测均失败于 "f2 ... is not implemented" 桩，`cargo test f2_` 0 passed/4 failed；格式通过）；☑ 最小实现（2026-10-19：CLI 双写法分流+D10 副作用前拒绝、install_native_body 提取+唯一 build 选定、拒绝门 5 处接线；f2 批 4/4、f1 套件、格式全绿 `target/f1-manual-20261019-213620.log`；rollback/system_install 门为记录的后续加固点）；☐ 用户验收（含网络 e2e `tests/e2e/cli/test_f2_exact_python_install`，用户道）；☐ 两轴静态审查。

**先完成：** F1。新增安装复用R06，不重做语言库。

**用户看到什么：** 缺项提示后，用户显式执行已有安装命令；完成后同一目录选择路径可找到它，再次安装会复用。下载失败明确报错，已有内容不受损。

**修改现有文件：**

| 文件与定位锚点 | 本阶段只改什么 |
| --- | --- |
| `cli/src/commands/install.rs:160` | 精确Python分支在旧`migrate_monorepo_lockfiles`、任务工具收集、远端resolve之前进入程序层；无参数只读配置完整版本，包安装留F3，保留原命令/输出。 |
| `src/core/programs/install/{request,context,runtime}.rs` | 复用normalize_intent、同次观测与`prepare_language_with_revalidation`；使用F1精确门，冻结来源/凭证/deadline，关闭声明/锁写入。 |
| `runtimes/src/plugins/core/python.rs:642/841`、`runtimes/src/plugins/core/python/native.rs:150`、`runtimes/src/plugins/core/python/native/pipeline.rs:419` | 提取接收既有OperationContext的native主体，继续使用原获取/证明/发布核。旧install_native_runtime会经native_operation_context重建deadline/取消/根，新程序入口不能调用该wrapper刷新上下文；旧wrapper仅留旧消费者。当前choose_release会取较大的distribution_version，新路径按完整pin唯一选定，多build/变体不自动挑最新。 |
| `runtimes/src/backend/mod.rs:3551/3680/4149/4362` | 在最早hook、rename、删写之前识别新共享对象并拒绝旧force/repair/postinstall/uninstall/purge等旁路；只保护新共享对象及其兼容路径，不重写其他backend。 |
| `runtimes/src/toolinstall/{install_rollback,system_install,tool_purgatory}.rs`、`runtimes/src/install_layout/resolver.rs:917` | 仍可达的回滚、提权发布、延迟清理和兼容路径统一消费同一拒绝门，不能在晚期才检查；不实现完整R07许可系统。 |
| F1的`programs/install/selection.rs`、`programs/shared/commit.rs`、`libraries/project/repository.rs` | 项目安装成功后接同一关联动作；全局下载不造项目；获取失败保留原关联，不新增另一套提交。 |
| `src/core/programs/{mod,install/mod}.rs`、`src/core/libraries/language/mod.rs` | 注册新增应用/拒绝门与测试，有限公开给CLI。 |

**拟新增文件：** `src/core/programs/install/apply.rs`负责显式安装编排；`src/core/libraries/language/mutation.rs`负责旧路径识别及保守拒绝，不能发放删除许可。其余来源/验证能力复用`src/core/artifact_snapshot.rs`、`native/provenance.rs`与R06，不新写下载器或签名算法。

**接口交接：** `install(request) -> OperationReport`只接受R01归一化请求及F1精确门通过的输入；已有精确可用对象直接复用，缺项才调用R06获取闭包。项目场景调用F1的关联应用，全局场景只发布内容。`mutation`向旧调用者返回“非受管目标/明确拒绝”，不以布尔判断伪装物理删除权限。

**实施动作（按以下顺序，完成后更新上方状态）：**

- 收拢普通install的Python窄路径，与F1使用同一配置和调用上下文；复用已经接到R06的`install_native_runtime`/`prepare_language_with_revalidation`，不重写下载和发布。
- 接入本次真正需要的来源、凭证、缓存和期限校验，优先复用现核；不等多来源/复杂认证组合全部完成。
- 从原R07抽取新共享对象的保守拒绝门，含兼容路径，在最早的hook、rename或删写前拒绝旧force/postinstall/uninstall/purge及延迟清理；F4完成前不开放新指定删除，完成后仅走F4受控入口。
- 项目使用的安装/选择成功后接上同一精确关联记录；纯全局下载不捏造项目引用，后续查询可以显示“尚无项目关联”。
- 首版不新增自动修改项目配置/锁的职责。旧入口若会隐式写这些状态，必须绕开该分支，或带入真实写域必需的既有原子/CAS保护；不能删掉计划上的R15前置却照样做未保护的多文件提交。

**复用的原任务：** R16语言部分 + R10必要部分 + R07最小拒绝保护。R09/R14/R15按本步实际读写域取必要部分，不以“整个横向任务全部完成”为前置。

**交付验证：** 新装后运行真实Python；第二项目复用同一payload；网络/校验失败不发布半成品；旧强制修改和删除路径触及新共享对象时在副作用前被拒绝。

**测试文件与必需断言：**

| 测试文件 | 合同 |
| --- | --- |
| 新增`src/core/programs/install/apply/tests.rs` | `f2_exact_install_reuses_and_records_only_real_projects`、`f2_failed_acquire_keeps_published_state`：获取次数、真实复用、项目/全局关联区别、失败后旧内容和选择不变。 |
| 扩现有`runtimes/src/plugins/core/python/native/pipeline.rs:473`的内联tests模块 | `f2_multiple_builds_are_not_latest_selection`：精确Python版本下多个build必须报告歧义，完整pin选中唯一制品。 |
| 新增`src/core/libraries/language/mutation/tests.rs` | `f2_legacy_mutation_is_rejected_before_effects`：实际旧入口的force、hook、回滚/延迟清理在副作用前拒绝；不能只单测判断函数。 |
| 新增`tests/e2e/cli/test_f2_exact_python_install` | 通过真实install安装完整PBS发行，进入项目可运行；再次安装和第二项目复用；不完整请求零获取，失败不产生Ready。 |

**阶段边界/退役条件：** 不支持的源码安装/包安装/配置编辑明确返回能力诊断，不回退python-build/pip/旧venv；不删除那些仍有其他消费者的整模块。F4前新共享对象所有物理删除拒绝，F4后只有其持有删除许可的入口放行；旧uninstall后补判断不能算完成。

## F3：配置里的一个真实 Python 包能导入

**状态：** ☐ 测试已写并注册；☐ 用户确认预期失败；☐ 最小实现；☐ 用户验收；☐ 两轴静态审查。当前全部未实施。

**先完成：** F2。完成后形成第一条实际可用的Python项目闭环。

**用户看到什么：** 在配置中声明支持范围内的包，选择安装后能真实import并运行其入口；A/B相同包内容复用，不同包选择不串用；没有额外建环境步骤。

**修改现有文件：**

| 文件与定位锚点 | 本阶段只改什么 |
| --- | --- |
| `src/core/programs/install/{request,context,apply}.rs`、`src/core/programs/shared/exact_version.rs`（apply与exact_version由F1/F2建立） | 增加精确包根与完整配置来源；语言、直接包声明全部先验证再获取；apply消费真实闭包并发布，不能只记根包名。 |
| `packages/src/python_project/resolve.rs`、`python_resolution/source/{registry_depot,registry_depot/closed}.rs`、`python_resolution/graph_check.rs` | 原位接prepare_project_registry、ResolveRegistryDepot、close_registry_plan；消费实际metadata及checked roots/edges，不复制求解/候选算法。只开放一个公开Simple来源及兼容wheel。 |
| `packages/src/python_wheel{,_plan,_install}.rs`、`python_wheel_install/materialize.rs`、`python_wheel_scripts/launcher.rs` | 原wheel图、ZIP/RECORD/scheme/冲突/脚本校验改消费受管共享输入和最终位置；只提取有真实新消费者的接缝，不放宽现有校验。 |
| `packages/src/python_environment/{build,runtime_target,resolved_build,runtime_bootstrap}.rs` | 新生产链以真实LanguageLease代替OwnedRuntimeSnapshot及完整runtime输入复制；原目标探测和构建核共用，旧消费者尚存时保留兼容薄层。 |
| `packages/src/python_environment/{runtime_layout,runtime_startup,seal}.rs`、`runtime_startup/installed.rs`、`prefix/verify.rs` | 适配库内包的真实启动/导入/入口与最终核验；受管链接有专门receipt，不能全局放松nofollow/nlink。 |
| `runtimes/src/toolinstall/python_site_adapter.py` | 仅在既有启动适配确需时接选定包路径；不修改共享Python全局site-packages，不执行用户额外激活步骤。 |
| `src/core/libraries/project/{selections,repository,read_views}.rs`、`programs/shared/commit.rs`；`packages/src/python_environment/model.rs`及F1列明的codec文件 | 在同一权威保存精确根、节点、真实依赖边、来源制品pin及layout→language/tree关联；当前扁平typed refs+graph摘要不足以供F4反查，不再造另一refs表。 |
| `cli/src/commands/install.rs`、`cli/src/commands/deps/install.rs`、`packages/src/install.rs` | 受支持Python请求进入同一原生程序；新分支退出DepsEngine/外部manager、旧sidecar和自动GC调用；保留其他生态尚存消费者。 |
| F1的`src/core/programs/query/{snapshot,paths}.rs`、`src/core/programs/install/selection.rs`；`cli/src/commands/{hook_env,env}.rs`的选择结果接缝 | 退出“包声明尚不支持”分支，读取已发布精确组合，投影包入口/加载适配，目录应用提交完整包引用。安装成功必须接回真实进目录使用链，不能只改install而漏选用。 |
| `src/core/programs/install/mod.rs`、`src/core/libraries/{package,project}/mod.rs`、`packages/src/python_environment/mod.rs` | 注册下列模块及测试，不新增第二个environment入口。 |

**拟新增文件：**

| 文件 | 单一职责 |
| --- | --- |
| `src/core/programs/install/{plan,resolve,layout,verify}.rs` | 明确输入计划、委托真实闭包、组织内部适配、验证可发布结果；不复制协议核，不引入完整build系统。 |
| `src/core/libraries/package/{blob,tree,manifest,lease,layouts}.rs` | 精确制品、已验证不可变wheel内容、持久事实、实际对象保护、无ProjectId的共享适配组合。先预留最终位置再生成位置相关入口。 |
| `packages/src/python_environment/shared_runtime.rs` | 单一共享runtime桥，持LanguageLease贯穿准备/校验；只生成必要轻量适配，不复制解释器和标准库。 |
| `src/core/libraries/project/package_graph.rs` | 将已核验闭包投影为有界精确根/节点/边数据；持久提交仍进入F1的catalog，没有单独可写引用权威。 |

**复用而默认不改：** `src/core/artifact_snapshot.rs`及`artifact_snapshot/cache/publication.rs`的VerifiedArtifact；`packages/src/python_wheel_source.rs`的inspect_owned_wheel；`packages/crates/pakro-index/src/{python_source,python_tags,python_metadata,python_requirements,python_markers}.rs`及`simple/client.rs`；`pakro-resolver`原纯核；R05 IO。旧`pakro-store/src/store.rs::unpack`的`.pakro-complete`和rename失败不能当新wheel Ready/并发胜出证明；旧Store/Refs留给现有其他消费者。

**接口交接：** `resolve`返回原有ClosedRegistryGraph及真实metadata持有者；`layout`消费闭包、VerifiedArtifact/包租约和LanguageLease，返回已核验的PreparedLayout；`apply`只提交这份真实结果到F1同一catalog。真实language/package/tree/layout保护须覆盖catalog CAS；跨阶段若需重取，按F1项目→对象→catalog顺序取得并复核，不能校验后释放再按旧描述加引用，也不能持对象锁反向等项目锁。完整版本/来源/摘要和依赖边可由F4读取，图摘要不是边列表。F1查询随后按已发布包组合选择；已有内容选择不重新求解、不联网。

**实施动作（按以下顺序，完成后更新上方状态）：**

- R08只先接这条路径需要的原始制品/展开内容存取；源码构建产物管理不作为前置。
- R10/R11接一个公开Simple来源、真实metadata与完整依赖选择，复用原解析/求解核，不重新写算法，也不忽略样本声明的传递依赖。
- R12复用wheel路径、RECORD、安装与脚本核；R13用LanguageLease替代这条生产链的完整runtime复制，做必要包路径/启动适配。
- 改接实际install/deps install消费者，使用户命令可达；保存精确根、真实依赖闭包及对象关联，不能只记根包名。R09/R14/R15纳入本步真实写域的必要保护；未实现的完整锁/恢复能力明确不支持，不伪造结果。
- 若实现需要同时改变配置、锁和权威引用，必须补相应提交保护，或继续收窄写入范围；不能把多个未保护写操作称为“以后修bug”。

**复用的原任务：** R08/R10/R11/R12/R13/R16/R23的必要部分，合成一条用户命令到真实import的路径；不是把这些模块的全部扩展要求一次做完。

**交付验证：** 一个真实wheel及其必要依赖可导入；A/B共用同一包内容和Python标准库；不同包选择互不污染；不兼容包、损坏文件和安装失败不冒充成功。其余平台/源码/原生扩展组合不算已验证。

**测试文件与必需断言：**

| 测试文件 | 合同 |
| --- | --- |
| 新增`src/core/programs/install/resolve/tests.rs` | `f3_exact_roots_preserve_real_dependency_edges`：直接根必须精确，metadata传递范围真实解析，记录闭包的节点/边/pin。 |
| 新增`src/core/libraries/package/{blob,layouts}/tests.rs` | `f3_verified_content_is_shared`、`f3_failed_layout_is_not_ready`：同内容复用、实际赢家重验；损坏/不兼容/中断不发布假Ready。 |
| 新增`packages/src/python_environment/shared_runtime/tests.rs`，扩F2的`src/core/programs/install/apply/tests.rs` | `f3_shared_runtime_has_no_full_copy`、`f3_failed_apply_keeps_old_selection`：真实语言租约与标准库来源、关联失败保留旧选择、项目声明未改。 |
| 新增`tests/e2e/packages/test_native_install_shared_wheel`、`test_native_deps_install_preserves_declarations`、`test_native_deps_no_manager` | 真实CLI安装固定完整版本、带传递依赖和console script的公开wheel；真实import/版本/路径/入口，A/B复用且选择隔离，不调用pip/uv或自动prune。 |

**阶段边界/退役条件：** 不创建`package/builds.rs`或源码构建协调器；sdist/URL/Git/local/editable/Conda、deps add声明编辑、升级、完整可移植锁/冷恢复、多文档事务和完整平台矩阵后移。现有完整pin必须遵守，不支持的锁模式拒绝，不能降级重求解。只有这条真实新链接通并由用户验收后，才删除其重复旧正文；`runtime_snapshot.rs`和旧store尚有消费者时不整删。

## F4：查看库内容及关联，删除明确指定的语言和包

**状态：** ☐ 测试已写并注册；☐ 用户确认预期失败；☐ 最小实现；☐ 用户验收；☐ 两轴静态审查。当前全部未实施。

**先完成：** F1–F3的真实内容及持久关联。用户已明确纳入首版，不以旧R21整项完成作为前置。

**用户看到什么：** 查看Pakro已关联的项目、库内语言和包的完整版本，以及谁使用谁；明确指定对象删除，有项目或依赖在用就列出并拒绝，没有引用才允许删除。

**修改现有文件：**

| 文件与定位锚点 | 本阶段只改什么 |
| --- | --- |
| `cli/src/commands/current.rs:43`、`ls.rs:152` | 改接只读选择/已发布实例与包事实，保留声明未安装状态和原输出职责；不隐式注册/联网，不顺带接远端比较或prunable。 |
| `cli/src/commands/project.rs:71/195` | project ls/status改接持久登记及真实关联，替换读索引失败返回空集、仅检查旧lock/venv的逻辑；不顺手删除init/attach等混合文件其他职责。 |
| `cli/src/commands/installs/ls.rs:44` | 展示实际库实例、完整版本/build/目标及使用者；同版本不同实例不折叠。 |
| `cli/src/commands/installs/mod.rs:32`、`cli/src/commands/installs/select.rs:34` | 父入口的新共享库ls不再被旧experimental/identity布局开关挡住；旧migrate职责保持。首版select对新共享目标明确拒绝旧全局版本链接切换，不冒称已接同catalog；完整辅助选择接线后移，F4删除的精确实例由uninstall请求明确承载。 |
| `cli/src/commands/uninstall.rs:61/162/175/220` | 新共享对象在旧backend前分流；退出版本前缀匹配、直接uninstall_version、删除后才改选择的旧路径；`--all`不能扩大本次目标。包目标必须接到真实删除入口与usage测试，不能只做内部API。 |
| F1的`src/core/libraries/project/{repository,read_views,selections}.rs`、F3的`package_graph.rs`；`packages/src/python_environment/model.rs` | 同一权威上增加有界反向引用/引用链投影；protecting_references的扁平集合不是“谁在用”的完整输出，不另建引用计数权威。 |
| F1的`src/core/programs/query/{mod,snapshot,paths}.rs`、`programs/shared/commit.rs` | 扩只读视图及删除结果所需单catalog更新，不另起快照/登记/通用恢复系统。 |
| `src/core/libraries/language/{repository,manifest,lease}.rs`、F3的`package/{blob,tree,manifest,lease,layouts}.rs` | 复用实际列举/manifest/租约，支持目标身份复核和删除状态；不直接以目录名当安装事实。 |
| `packages/src/python_environment/prefix/observer/cleanup.rs:325` | 只在确需时提取共用的逐项删除/目录绑定内核；原函数的权限属于未提交暂存，不能直接套给已发布对象，原暂存检查须保留。 |
| `src/core/programs/mod.rs`、`src/core/libraries/{language,package}/mod.rs` | 注册下列模块与测试；CLI只取得请求/报告，不能得到任意路径删除权限。 |

**拟新增文件：**

| 文件 | 单一职责 |
| --- | --- |
| `src/core/programs/query/{inventory,projects}.rs` | 组织语言/包实例和项目关联查询，明确完整性/缺失/损坏；包行与语言行区分，使用同一事实来源。 |
| `src/core/programs/uninstall/{mod,select,reachability,reclaim}.rs` | 选本次精确目标、列阻止引用、请求真实删除许可、执行并报告；没有全库扫描候选/prune或项目声明改写。 |
| `src/core/libraries/language/removal.rs`、`src/core/libraries/package/removal.rs` | 私有RemovalPermit持实际稳定EX、目录/manifest身份和状态；取得后再次查最新引用，删除仅覆盖本次受管目标，不能删除稳定锁文件。 |

**接口交接：** query消费F1/F3已提交事实，返回项目/语言/包记录及使用者链；`select_targets`只产生唯一精确目标描述；`plan_removal`返回该目标的阻止原因；`acquire_removal_permit`在与新增引用共用的保护下重读引用/对象；`capture_and_remove`消费不可伪造许可并报告实际结果。计划、路径、hash都不能单独授予删除权。

**命令边界：** 保留`ls/current/project ls/project status/installs ls/uninstall`各自职责，不新增统一find或另一套install/uninstall命名。包参数复用现有包请求解析与完整运行时/包身份，不以`ToolArg`前缀猜测包，不因为绑定某语言就连带删除语言。具体参数转换和usage测试属于本步CLI文件的必交付内容；多实例需明确定位，不能建议用`--all`绕过歧义。

**实施动作（按以下顺序，完成后更新上方状态）：**

- 复用R04关联模型、R05受控IO/锁及R06语言保护，接通最低持久登记与完整直接/间接引用视图。查询项目位置/状态及其语言/包，查询语言/包的精确版本、实例和使用者；不扫描整台电脑猜项目，也不只看当前目录。
- 从原R18/R26提取上述本地查询，从R09/R21/R25提取指定对象删除必需部分。沿用既有查询和uninstall职责，包级参数接线时明确，不在计划中虚构新CLI拼写。
- 删除按完整版本和实际实例定位；同名同版本多发行/变体有歧义时让用户明确目标。有项目、保留的包或其他依赖引用时，列出项目/依赖及引用链并拒绝，不偷偷改项目配置以消除引用。
- 在与新增引用共用的对象保护下，取得排他保护后重新核对最新引用和实际对象，只删除本次明确目标；Pakro正在使用，或登记损坏/项目暂不可访问导致无法确认目标引用时，报告受影响范围与阻止/未知原因，不把未知当无引用。
- 不连带删除其他包、语言、项目源码或配置，不自动递归删依赖/运行prune。删除失败明确报告实际状态，不把半删对象当可用；保留完成本次受控删除必需的状态处理，不先建完整批量GC/故障矩阵。

**交付验证：** 重启后仍能查询已知项目/语言/包及关联；项目引用或包依赖时列明并拒删；无引用目标真实删除且其他对象不变；未知关联不能当空，删除与新增引用竞争不误删。外部进程完整生命周期仍属后置范围，不在此夸大为已覆盖。

**测试文件与必需断言：**

| 测试文件 | 合同 |
| --- | --- |
| 新增`src/core/programs/query/{inventory,projects}/tests.rs` | `f4_library_relations_survive_restart`、`f4_query_is_readonly_and_corruption_is_not_empty`：重开真实库查询双向关联，空/缺失/损坏区分；无写入、联网、登记或修复。 |
| 新增`src/core/programs/uninstall/reclaim/tests.rs` | `f4_references_report_chain_and_block`、`f4_exact_unreferenced_target_only_is_removed`：项目、传递依赖、保留包的引用链完整；无引用目标实际删除且其他对象不变。 |
| 新增`src/core/libraries/{language,package}/removal/tests.rs` | `f4_new_reference_and_live_lease_block_removal`、`f4_unknown_or_partial_delete_is_not_success`：真实锁竞争、活租约、未知关联和部分失败不会误删/报Ready；稳定锁仍在。 |
| 新增`tests/e2e/cli/test_f4_library_query_and_uninstall` | 实际CLI分别查询项目/语言/包、查看关联、尝试被引用目标及无引用目标删除；同版本两build时模糊请求拒绝，参数明确一个实例后只删除该实例。包入口与实例参数确实可达，不以打印预设结果代替。 |

**阶段边界/退役条件：** 不创建uninstall/project_edit.rs、全库prune、强制删除、自动递归依赖回收、项目源码/配置清理或完整恢复矩阵；失败对象必须停止作为Ready暴露，但不扩为全库修复。已完成的历史安装记录不永久保活；仍有实际项目/包依赖或在途恢复保护才阻止。旧CLI删除正文只在对应消费者真正替换且验收通过后退出，不批量删其他生态实现。

## 第一版之后再按使用反馈推进

以下是后移，不是永久删除；恢复相应功能时，同时补它真正需要的保护和测试。

| 后移内容 | 原任务 | 为什么不挡首版 |
| --- | --- | --- |
| 自动编辑声明、完整可移植锁/恢复、通用多文件事务及崩溃恢复矩阵 | R09/R14/R15、R17/R22–R24的扩展部分 | 最低精确版本/项目依赖关联和实际写入保护已纳入F1–F4，后移的是其余完整变更能力。 |
| 批量prune、复杂回收、强制变更及完整中断恢复矩阵 | R21/R25及完整R07的其余部分 | F4已包含受引用保护的指定删除；首版不自动GC、不连带清其他对象。 |
| 其余查询输出、远端比较、诊断与所有辅助命令接线 | R18/R19/R26/R43的其余部分 | 库内项目/语言/包及关联查询已纳入F4，其他扩展按使用补接，不合并原职责。 |
| URL/Git/本地源码、源码构建、editable、Conda、语言源码安装 | R27–R34 | 第一版仅承诺指定来源的兼容wheel和预编译Python；不支持的输入明确诊断，不偷偷调用外部管理器。 |
| 多私有源/认证组合与完整跨平台矩阵 | R10及各生态矩阵的扩展部分 | 单一已支持来源仍保留凭证与来源检查，未覆盖组合不伪称支持。 |
| 全仓旧链退役、工程入口拆分、生成资源与依赖大清理 | R39–R44 | 不为清理而先重构半个仓库；当前消费者确已替换时才做局部退役。旧数据清理另需用户授权范围。 |
| Java、Node、IDE/外部进程全生命周期 | 原M-PY/M-JAVA、R38/R35、R20 | 先完成可用Python，再按既定生态顺序扩展；首版不是整个Python阶段或产品最终验收。 |

## 缩减测试与防护工作的方法

- 已实现的摘要/来源校验、受控解包、锁/租约和发布内核直接复用，不拆掉后再等事故重写。
- 当前确实会执行的写入不能覆盖用户文件、修改其他项目共享内容，半成品不能报成功；不支持的修改/删除入口直接拒绝，不先实现全套恢复能力。
- 每个阶段交一条真实使用链及关键失败例；只验证本次改动和受影响的旧调用，不重复跑未变化的全套，不提前扩展跨平台/极端故障矩阵。
- 使用中发现问题：先定位，补最小复现，再修对应路径。不要顺手把未来所有边界都加成当前阶段的前置。
- 编译、测试和格式检查由用户手动运行，Agent在实际接口存在后给准确命令并读取日志；0 tests、全ignored或仅编译成功不算用户功能交付。
- 保持同一构建配置，继续采用已有关闭增量缓存/调试信息的手动方案，不无故生成多套产物；不自动清理用户数据或提交Git。

## 各阶段的手动验收交付

本节是未来实施时必须交付的测试入口合同。**F1的`tests/scripts/f-manual-check.sh`与`tests/scripts/f1-test-cases.txt`已于2026-10-18随测试注册创建；当前桩阶段预期全部`f1_`用例失败（RED），通过不算交付。F2–F4脚本仍不存在，当前不要执行对应命令。**原R06脚本及通过证据保留，不把它改成新阶段运行器，不无故重跑R06全组。

F1实施时新增`tests/scripts/f-manual-check.sh`及`tests/scripts/f1-test-cases.txt`；F2/F3/F4分别增对应`f2-test-cases.txt`、`f3-test-cases.txt`、`f4-test-cases.txt`并扩同一脚本。每份清单登记本阶段真实包、lib/bin、完整测试名和明确E2E脚本；复用已有cargo/`tests/e2e/run_test`，不新建测试框架。脚本只允许用户手动调用。

- 用例存在并注册后，先`--list`逐个核对完整名称再执行；0 tests、全ignored、只编译、仅子进程helper都不能记为通过。真实测试依赖缺失明确停下，不跳过后仍打印成功。
- 固定`CARGO_TARGET_DIR=target`、`CARGO_INCREMENTAL=0`、`CARGO_PROFILE_DEV_DEBUG=0`、`CARGO_PROFILE_TEST_DEBUG=0`和`--all-features`，同阶段保持包集合与构建配置；不自动补跑release或换target目录堆出第二套产物。
- 只跑本阶段表列合同、实际受影响的既有回归、必要非测试编译与本次文件格式检查；不把所有历史B0/B1/R00–R06或整个未来矩阵加入每次验收。
- 保存完整输出、阶段/模式、实际用例数、退出结果到`target/fN-manual-时间.log`；支持`--format-only`只处理本阶段实际变更文件，不能格式失败后强迫重编所有测试。
- 真实归档沿用用户已指定的PBS 3.13.16路径及摘要（见HANDOFF），使用隔离库和项目；wheel真实输入由用户指定完整版本/来源或授权下载后才纳入，不能猜路径/偷偷下载。模拟夹具仅验边界，不能冒充完整真实发行/包导入通过。

| 阶段 | 交给用户时必须说明“现在要测什么” | 实施并注册后的预定入口 |
| --- | --- | --- |
| F1 | 进A/B目录是否选同一份Python，缺项是否只提示，退出路径是否恢复，重启后关联是否还在。 | `bash tests/scripts/f-manual-check.sh f1` |
| F2 | 显式安装是否真实成功、重复是否复用，失败是否保住旧内容，模糊版本和旧强制路径是否提前拒绝。 | `bash tests/scripts/f-manual-check.sh f2` |
| F3 | 包和依赖是否能真实导入/执行，A/B是否共享且不串包，源码/锁不被改，失败不冒充成功。 | `bash tests/scripts/f-manual-check.sh f3` |
| F4 | 库内容和关联能否查清；有人/依赖在用是否列明拒删，无引用是否只删除指定对象。 | `bash tests/scripts/f-manual-check.sh f4` |

实际交付时提供带`cd '/Users/fuxinao/文件/project/pakro'`、真实输入前提的完整可复制命令，并依据当时已实现接口核实清单；这里的规划入口不等于代码已经存在。

## 已完成基础与证据

| 已完成 | 可复用成果 |
| --- | --- |
| R00–R03 | 基线、程序请求/上下文、真实配置观测与项目快照。 |
| R04–R05 | 内容身份与项目选择分离、受控IO、稳定锁和统一内核。 |
| R06 | 唯一共享Python、实际租约、发布/复用、来源证明与真实共享验收。 |

R06合并150项不同测试通过；三包非测试编译、37文件格式与diff检查通过，详见[交接记录](HANDOFF.md)和[完成日志](log/2026-10-18-4-R06共享语言库.md)。原详案的完成状态与文件/测试合同原样存档，不把后移任务写成已完成。

**下一步不是开工。** F4和精确版本要求已按用户指令写入；仍需用户明确指定开始实现，不能借计划重排自行启动R07或F1–F4。
