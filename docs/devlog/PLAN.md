# Pakro Python完整化实施计划：P1–P9

方案确认与P1细化：2026-10-24（Asia/Shanghai）。源码基线：`3d2a6c291`；已有F/S成果与证据见[交叉盘点](ARCHITECTURE-REVIEW-2026-10-24.md)。架构权威是根[主架构与Agent规则](../../Pakro主架构文件与Agent规则.md)。

**授权状态（2026-10-25）：用户确认P1本机macOS arm64产品验收通过，自动多进程运行器禁用，授权将既有P1实现、测试和收尾文档提交Git；本轮不改代码。** 长测试继续由用户终端执行；通用监督器不追加为P1产品验收门槛。整个项目停用dev-flow，不自动开始P2；跨平台、完整ISAT业务及后续阶段边界保留。

## 当前方案与阶段顺序

配置表达用户意图；统一`pakro.lock`保存可移植精确结果；catalog是本机选择和引用权威；manifest描述实际内容和所有权。保持三类程序、三种库，CLI不直接写删库对象；以真实F/S生产链为复用基线，不为填满旧文件表再建一套实现。

| 阶段 | 本阶段交付 | 前置与完成条件 | 旧R追溯（只对应剩余范围） |
| --- | --- | --- | --- |
| P1 统一当前Python生产路径 | 真实入口收敛、查询/目录事实一致、旧Python写路径早拒绝、大图/FD/并发基线。 | 保留F/S全部现有能力；只读无隐式获取/写入；详见下方P1。 | R01/R07/R16/R18/R26及局部R40/R43 |
| P2 精确锁与严格恢复 | 统一锁读写、锁/catalog最小受控提交；先wheel，再已有纯Python源码包。 | 依赖P1真实入口；冷/暖回放不重新求解、不改锁，拒绝不支持/不匹配条目；源码输入与实际输出分开核验。 | R11/R14/R15必要部分 |
| P3 完整项目变更 | 同一提交核加入声明；use/deps add → deps remove/unuse → upgrade；项目移动、解除关联、失联保护。 | 依赖P2提交/读一致协议；准备失败保留旧状态、最后一个包根能提交、精确目标升级不破坏其他项目，不自动prune。 | R09/R17/R22/R23/R24/R15剩余部分 |
| P4 显式回收与恢复 | 指定删除完善、显式prune/预览、持久回收进度、中断续行与明确修复边界。 | 依赖P3正确引用释放；从真实项目/在途根标记，无根环可回收，未知不当零引用；4-pyc例外不推广成force。 | R07/R21/R25剩余部分 |
| P5 完整来源接入 | 私有registry → 精确URL → Git固定快照 → local不可变快照及项目自身依赖。 | 复用P2–P4合同；每来源同交付查询、锁、严格恢复和引用/回收，不只完成下载。 | R10/R27–R29 |
| P6 完整构建 | 支持的归档、受控C/C++/Rust扩展、解释器源码配方、Linux原生布局。 | 先保持P2已有源码恢复；工具链/SDK为固定输入，实际强制工具准入而非单纯关闭deny_process；不同重建输出不偷改锁。 | R12/R13/R30/R31/R33剩余部分 |
| P7 editable | 活源码绑定、可写范围、metadata失效、扩展重建、卸载。 | 依赖local及已支持构建；editable产物不进入普通不可变缓存，不删除用户源码。 | R29/R32 |
| P8 Conda | 独立求解语义接共同三库/事务/锁/回收，检查混合PyPI冲突。 | 共同合同稳定后可并行设计；唯一Python payload，文件/分发冲突写入前拒绝，整图可恢复。 | R34 |
| P9 Python收尾与完整验收 | 查询/辅助入口、Python旧链退役、工程/Schema/生成资源收尾及真实项目/平台总验收。 | 汇总各阶段证据；Python完成门通过后才Java，Java完成后才Node。 | R18/R19/R26/R39–R44及R45的Python部分 |

P1→P2→P3为最先实施主线。平台、性能、故障与局部退役随阶段开展；工程入口拆分可并行规划，不成为P1整仓重构前置。P5–P8每新增一种对象或来源，必须同步锁/查询/引用/回收，不能让P4只认识旧wheel。旧R/F/S“后移”不永久取消相应新阶段，也不自动授权实施。

### Python完成门

1. 安装、目录选用、增删依赖、精确升级、查询、解除关联、卸载和显式回收形成共同事实上的完整生命周期。
2. wheel、源码、支持来源和Conda严格冷/暖恢复，以及并发、中断、外部改写/损坏有实际证据；查询只读，不在恢复窗口拼接新旧事实。
3. 同语言实例只有一份实际内容，兼容包可共享，各项目选择隔离，删除/升级A不破坏B。
4. 既定Python范围包括源码包、解释器源码、editable、私有/URL/Git/local及Conda；明确支持的版本/后端，不能笼统声称所有包均支持。
5. Linux/macOS、x86_64/arm64、glibc/musl、CPython/PyPy按适用组合列矩阵并验证；Windows不在当前目标，未测组合不称通过。
6. 补齐ISAT完整图复用、冷恢复和已定义业务断言，用独立小项目覆盖源码/editable/Conda；验收设施摆脱本机路径及测试binary哈希。

IDE/外部解释器完整生命周期仍单列后置，路径描述不是运行租约；Go与独立Rust生态不恢复。旧测试数据清理只处理明确对象，不从P1文档获得删除权限。

## P1：统一当前Python生产路径

**状态：P1 本机 macOS arm64 产品验收通过，自动多进程运行器禁用。** 用户已核对最终构建和定向E2E原始日志（all exit0），此前精确安装CLI正例及A14同配置对照继续有效。通用监督器不作为额外产品验收门槛；长测试由用户终端执行。按本次授权提交Git，本轮只同步文档，不改代码，不自动进入P2。

### 2026-10-25定向复核

最新用户手动日志`p1-recheck-manual-all-20261025-002758.log`已成功构建（1分07秒），两条定向E2E通过，all exit0。此前E0433已消除；用户现确认P1本机产品验收通过。自动多进程运行器仍禁用，通用监督能力不作为本阶段额外门槛。

- monorepo/task：维护前收集完整扩展工具/依赖/包组，缓存同次计划供实际安装复用。含Python的monorepo写域整体拒绝（包括显式NonPy过滤），静态NonPy扩展保留；未知/remote工具元数据早报错，工具规划不求值无关run/description模板。
- which：默认先检查共享Ready入口，使同名时与env/hook共享PATH优先级一致；共享查询失败时仍允许已核实的NonPy companion，显式--tool保持定向。
- 监督能力：旧快照追踪不能覆盖快速setsid/reparent；默认tree现在返回125、coverage_complete=false、survivors=null且不启动目标。显式single-process仅供可信非特权无子进程命令，macOS在exec前设置RLIMIT_NPROC软硬0，内核阻止fork/spawn。严格任意多进程树执行仍未实现，自动运行器保持禁用；用户已确认不将通用监督器追加为P1产品验收门槛。
- 验收：Rust格式、Shell/Python语法和短监督能力实验通过；用户长构建与2条定向E2E已通过（日志002758）；无需无变化重复运行。入口`bash tests/scripts/p1-manual-recheck.sh all`，复用target/all-features/debug=0/incremental=0，不另建release/target。

### 本轮审查修复与补证

1. 配置Python包组纳入install/use实际尾部执行计划，未迁移组合在安装/声明保存前拒绝；独立非Python正例保留。
2. asdf exec-env直接source及ExternalPluginCache纳入只读保护，动态bin路径/环境错误向上返回，不能静默丢字段后成功。
3. env依据配置加CLI追加后的有效工具集分流，非Python追加不撤掉项目Python。
4. which先核实已安装非Python附带命令的实际路径，再判断共享Python；缺Python不挡已有companion。
5. alias去选项后识别Python归属，再统一拒绝选项；无参查询和环境版本覆盖不再丢失声明。
6. 监督器以私有session和已观察后代为清理对象，TERM后无论组长是否退出都继续KILL/复查，覆盖正常退出遗留和监督器中断。

补证：冷空库精确Python CLI安装已通过固定官方tag/校验和/真实归档的本地传输夹具；A14以原3d2a6c291的生产选择核和当前选择核运行同一64包夹具，比较完整query与apply入口。详细结果以TESTS本轮表及日志为准；不将有限覆盖推断为所有组合通过，不自动进入P2。

### P1范围、保留能力与排除项

P1只交付三项实际结果：现有Python安装链有唯一生产编排；本地查询/目录使用同一事实；未迁移Python写路径在首次副作用前拒绝。附带可复现的大图耗时/FD/并发基线及本次收敛的防回退证据。

必须保留F1目录选择/F2官方预编译精确安装/F3 wheel/F4指定查询删除，以及S的registry纯Python `.tar.gz`、legacy、初始/动态构建依赖、同solver续传、源码与实际wheel双证据和native COW。保持来源pin、build歧义、实际内容核验、PYTHONDONTWRITEBYTECODE、soft FD初始化及原严格Locked包安装拒绝。现有`--restore-bytecode`精确特例及其dry-run/quiet早分流继续保留，只用隔离夹具回归，不再次操作用户实际4个pyc。

P1不写统一项目锁、不实现配置/锁/catalog通用WAL，不接通use/add/remove/unuse/upgrade新事务，不新增批量prune、来源/编译器或生态。不改三库目录/格式，不重构lease或稳定锁持有模型，不开放跨操作hash缓存，不扩大300秒选择期限或4096软FD目标。工程入口拆分只记录依赖，整仓清理不混入P1。

### P1.1 入口与结果合同

| 请求/结果 | P1合同 |
| --- | --- |
| 不适用共享Python | 依据解析后的实际目标/provider判断，才允许非Python既有路径；不是“结果为空”或“共享库尚未初始化”。显式非Python查询不必读损坏的Python库。 |
| 无当前Python声明 | 目录不选择Python、不登记；显式版本查询仍可只查本地库存；无目标时不拿某个旧Python冒充当前选择。 |
| 语言已就绪 | 已核验唯一实际语言实例，可返回安装根。并不自动代表项目包图完整。 |
| 环境已就绪 | 语言、当前请求所需包图/layout及命令入口均符合同次观测与pin；返回描述，不授予未来进程租约。 |
| 缺语言/缺包图/缺命令 | 分别报告缺项；不得获取、自动修复或落回旧Python/外部manager。 |
| 无效/歧义/损坏/不可读 | 保留错误类别与必要候选身份；不折叠成None、空库或不适用。观察过期、取消、超时也有明确结果。 |
| 声明与已提交选择不同 | 同时区分当前声明、匹配的已有内容和已提交关联；查询不“同步”catalog，原引用到新选择成功后才更新。 |
| 显式目标覆盖 | `env python@X`、`which --tool=python@X`、`bin-paths python@X`消费X，不偷用cwd的Y；与X不兼容的Y包图不拼入结果，明确给出目标/包绑定不兼容或缺项。覆盖只影响此次查询，不改项目声明/选择。 |
| Python版本筛选 | 安装及项目声明保持完整精确版本。已有路径查询的前缀仅作本地候选过滤，不访问远端；单路径输出要求唯一实例，多候选列明歧义，不任取最新build。未支持参数组合明确诊断，不回旧Python。非Python原查询规则保留。 |
| 只读CLI初始化 | 共享Python本地查询在handler之前也不得自动更新、刷新registry、安装插件、迁移、清理或写Tracker。按真实解析命令/作用域分流，保留必要的只读内置元数据加载，不依赖字符串搜索。 |

查询按所需事实加载，不能强迫每个字段重复完整环境核验：`where`返回语言payload安装根；`which`返回当前有效的解释器/console script入口；`bin-paths`返回有效bin集合；`current`保留版本输出并区别环境是否完整；`project status`区别声明/登记/可用性。语言存在而包缺失时，where仍可正确定位语言，环境查询明确缺包。

普通查询对缺项/无效等沿各命令原有文本/JSON和退出约定输出；必须单独验证非零错误与状态列表两类合同。hook可非致命警告，但不能输出新的成功选择；撤销前一目录的Pakro PATH/自有字节码变量差异，不能释放持久引用。JSON stdout不混入诊断，诊断走stderr。

### P1.2 具体文件与改动落位

以下保留P1开工前的逐文件合同；实际落位、新增必要接缝及局部退出详见[FILES当前P1表](FILES.md)。路径相对仓库，未迁移的后续能力不因本表而开放。

**A. 统一本地查询和目录应用（先做）**

| 文件 | 当前锚点/问题 | 拟改内容及边界 |
| --- | --- | --- |
| `src/core/programs/query/snapshot.rs` | `SelectionReport`、`select_existing`只表达语言；完整包匹配另在directory。 | 扩展请求作用域、明确目标和类型化状态，复用原语言/pin核；承接按需完整环境匹配，区分语言和环境就绪。 |
| `src/core/programs/query/paths.rs` | `SelectionPaths`、`selection_paths`只投影语言。 | 投影同一结果的payload根、有效bin、解释器与有归属的console script；不读取/安装另一套事实。 |
| `src/core/programs/directory.rs` | `select_directory_python`、`select_observed`吞错误为None，包图匹配/打印/commit混合。 | 匹配下沉query，保留目录适配；把只读选择与已有内容关联应用分开，取消易误用的公共commit布尔语义；冻结一次原总期限，内核不打印/吞错。 |
| `src/core/programs/library.rs` | `observe_current_project`、`local_project`、`current_observed`另建观测，ready只核验语言。 | 公共只读门面复用同次观察/上下文，按需返回声明、语言、环境、登记事实；不暴露writer/owner。 |
| `src/core/programs/query/inventory.rs`、`src/core/programs/query/projects.rs` | `inventory`、`projects`分别只读。 | 接共同上下文，区分空库/无当前选择/不可访问；空结果不决定路由，不登记或修复。 |
| `src/core/config/mod.rs`、`src/core/libraries/project/snapshot.rs` | `FrozenConfigDiscovery::from_explicit_options`、`observe_project`已有核；目录入口硬编码配置名。 | 集中绑定真实profile/global/system/ignore/no-config与CLI覆盖，消费既有冻结发现核，不另写parser。与旧Config接缝同时审查Tracker写入。 |
| `src/core/programs/install/selection.rs`、`src/core/programs/shared/commit.rs` | `apply_directory_selection`及语言/包选择commit。 | 只改接描述与观测。查询租约先释放，提交仍按项目→对象→catalog重获并复核；不持query对象锁反向取项目锁，不扩大单catalog写域。 |
| `src/core/programs/mod.rs`、`src/core/programs/query/mod.rs` | 公共facade与私有query注册。 | 只导出CLI所需请求/报告，优先扩既有library/directory门面；库IO/lease/删除许可继续私有。 |

**B. CLI与终端消费者（按A的同一合同接入）**

| 文件 | 拟修改锚点与具体行为 |
| --- | --- |
| `cli/src/commands/where.rs` | `Where::run`在旧resolve前按目标分流，输出真实语言安装根；Python缺项/歧义不走latest/旧目录。 |
| `cli/src/commands/which.rs` | `run`、`complete`、`get_toolset`接解释器/包入口；保留`--tool/--version/--plugin`和补全差异，补全仅列已有内容。 |
| `cli/src/commands/bin_paths.rs` | `run`、`list_bins`按Python/非Python分区再合并去重；保留bin names/JSON字段，受管损坏不能被目录扫描吞成空。 |
| `cli/src/commands/env.rs` | `Env::run`把self.tool的Python目标传入统一查询；所有输出格式共享结果，保留字节码变量优先级，只读不提交。 |
| `cli/src/commands/hook_env.rs` | `HookEnv::run`消费类型化结果，Ready关联应用成功后才输出新环境；保留原快速路径、PATH撤回和变量恢复，不获取缺项。 |
| `runtimes/src/shims/mod.rs` | `configured_non_python_provider`、`select_shared_python_command`、`install_missing_wrapper_command`保留明确provider归属；Missing不能变成None后触发旧Python lazy安装。 |
| `cli/src/commands/shim.rs`、`cli/src/commands/hook_not_found.rs` | `command_wrapper_for`、`which_shim`、not-found的run使用同一归属/结果；保留wrapper参数与非Python行为，不宣称外部进程生命周期已保护。 |
| `cli/src/commands/current.rs` | `Current::run`保留版本文本、补环境不完整诊断；混合请求不重新显示旧Python。 |
| `cli/src/commands/ls.rs` | `shared_library_query`不因special/mixed/空结果整体回旧路；给共享flag支持表，不支持的Python组合提前诊断。 |
| `cli/src/commands/installs/ls.rs` | `InstallsLs::run`按意图而非shared_names决定归属；显式Python空库仍输出共享空库；混合结果去除旧Python重复行。 |
| `cli/src/commands/project.rs` | 只改`ProjectLs::run`、`ProjectStatus::run`的只读投影，区分当前声明与已提交选择，不改init/attach或新增登记命令。 |
| `cli/src/commands/mod.rs` | `run_inner`在`install_state::init`、auto-update、registry/migrate/cache/purgatory及Tracker写之前，提供同一纯参数/只读目标判定通路，覆盖本地查询、将被拒绝的旧Python写请求及所有install-into。判定未完成前禁止自动维护，未知目标明确诊断；尊重别名、`--cd`、profile及更早的bytecode恢复独立分流。 |
| `runtimes/src/toolset/install_state.rs` | `init`/既有只读初始化接缝：受影响混合查询及准入判定阶段不能创建安装/跟踪状态；不重写其他命令全局初始化。 |
| `cli/src/commands/command_effects.rs` | 修正“隐式安装不算effect”的相反注释；查询与会提交关联的hook不能混称同一只读行为。只按真实行为修相应说明/分类。 |

混合请求须隔离Python与非Python读取；受影响的本地查询不得借非Python兼容分支重新加入旧Python、获取或写跟踪。无法保持某组合原输出及只读合同的，明确拒绝该组合，不静默降级。明确非Python独立请求继续原能力，不因cwd存在Python就一刀切拒绝。

**C. 保留真实F/S安装编排，收敛重复门面**

| 文件 | 锚点与拟改内容 |
| --- | --- |
| `src/core/programs/python.rs` | 以`prepare_current_project`、`PythonProjectInstall::install`、`install_exact_python`、`install_project_packages`、`resolve_and_commit`为生产基准；统一当前两安装入口所需意图/上下文，不丢完整根、runtime pin、source证明和S续传。 |
| `src/core/programs/install/request.rs`、`src/core/programs/install/context.rs` | 复用`normalize_intent`及同次观测核对；只接Install/DepsInstall当前需要的合同，不因Use/Upgrade/Add类型存在就开放声明写。 |
| `src/core/programs/install/apply.rs` | `install_exact_python`目前仅测试消费且只懂PBS输入。将重复编排委托生产核或退出已无consumer的私有正文，保留有价值断言；不得反向用它替换掉真实来源/pin处理。 |
| `src/core/programs/install/mod.rs` | 仅同步上述收敛/测试注册；不提前创建plan/acquire/layout/verify等完整R文件族。 |
| `runtimes/src/plugins/core/python.rs` | `install_shared_exact_in_context`、`install_shared_exact_with_report`、`install_native_body`维持真实runtime适配；新生产链不绕回旧Backend安装。旧`install_native_runtime/install_version_`暂留源码资产，不从新链可达。 |
| `src/core/programs/install/runtime.rs` | `prepare_language_with_revalidation`保持唯一获取/复用/实际来源核，只作必要接口适配，不重写R06发布。 |
| `cli/src/commands/install.rs`、`cli/src/commands/deps/install.rs` | `Install::run/run_exact_python_install`及`DepsInstall::run`统一交programs，保留当前显式runtime、bare/bound包及完整项目安装；list/explain的真只读行为与执行分开。 |

默认只复用、不改业务语义：`src/core/programs/install/resolve.rs`、`src/core/programs/install/build.rs`、现有registry/solver/wheel/source worker、语言/包发布与manifest核。不得为了合并门面重做这些内核；若接口确需变化，先列具体接缝，不自动扩范围。

**D. 旧写入口在首次副作用前拦截**

新增拟定私有文件：`src/core/programs/shared/python_entry.rs`及其`python_entry/tests.rs`，在`shared/mod.rs`注册，通过现有`programs/python.rs`提供有限纯分类/准入门面。名称为实施落位，职责固定为解析后目标与操作类别的判断，不读写库、不扫描shell命令字符串、不产生第二份配置权威。

| 入口/文件 | 具体截断位置与行为 |
| --- | --- |
| `cli/src/commands/use.rs` | `Use::run`在Toolset构建/补装/配置编辑前判断目标；Python use/remove/bound包等未迁移变更明确拒绝，不能等写配置后才失败。 |
| `cli/src/commands/upgrade.rs` | `Upgrade::run`在`migrate_monorepo_lockfiles`、远端候选和写入前判断显式或无参实际目标；P1不选择最新目标、不安排Python延迟清理。 |
| `cli/src/commands/unuse.rs` | `Unuse::run`在选取/初始化写目标和保存前拒绝Python声明移除；`--no-prune`不绕开未具备的事务。 |
| `cli/src/commands/deps/add.rs`、`cli/src/commands/deps/remove.rs`、`cli/src/commands/deps/mod.rs` | 先用既有纯`parse_package_spec`和provider标识判定，早于`install_missing_versions`与env/provider执行；pip/uv/poetry等Python适配也纳入，不用字符串黑名单。 |
| `cli/src/commands/deps/install.rs` | 保留新F/S分支；monorepo/only/skip等会进入旧Python provider的执行计划提前拒绝；list/explain不得借名补装。 |
| `cli/src/commands/prune.rs` | `Prune::run`及可被其他命令调用的`prune`函数，在Tracker/Trust清理和删除前核定目标；未支持Python批量回收拒绝，F4指定uninstall仍保留。 |
| `cli/src/commands/install_into.rs` | D01已取消，在`run`开头拒绝执行，早于Config/resolve/父目录创建；完整命令树/Schema退役留后续，不为它实现新安装。 |
| `runtimes/src/toolset/toolset_install.rs` | `install_all_versions_with_progress`在`ensure_config_plugins_installed`前检查旧批次及会补装的依赖，防未列CLI间接调用。 |
| `runtimes/src/backend/mod.rs` | `Backend::install_version`早于`prepare_install_version`接防遗漏门；`run_postinstall_hook`补真实生产接点，不等最终PythonPlugin才拒绝。 |
| `src/core/libraries/language/mutation.rs`及其`mutation/tests.rs` | 保留共享路径的底层拒绝核并核对真实调用。Force/PostInstall目前只见枚举测试直接调用，不能算全部生产已接；路径保护不替代前面的意图保护。 |

准入按实际目标判定：显式非Python且不会附带修改Python，继续既有能力；混合批次含未支持Python变更，整批在副作用前拒绝，不能先安装其他工具。缺省目标要用同次只读观测明确，不能先调用会安装/迁移的旧加载器再决定。未知/动态目标无法安全预判时明确诊断。任意自定义任务/脚本文本不做全局扫描封禁，整体task退役属于后续范围。

上表handler准入是最终目标复核，不代替`run_inner`启动前判定：在命令run之前已有安装状态初始化、自动更新、registry刷新、迁移和清理，因此同一准入逻辑必须覆盖这段前置链。`install-into`按D01对所有工具拒绝，非Python保留原则不为它提供例外；其他明确非Python请求保持原能力。

已接的底层保护默认只回归、不修改：`runtimes/src/toolinstall/install_rollback.rs`、`runtimes/src/toolinstall/system_install.rs`、`runtimes/src/toolinstall/tool_purgatory.rs`、现有语言/包removal。若发现新旁路，先提供实际可达消费者与最早副作用位置，再补具体文件。

**E. 测试与资源记录（代码实施时才创建/修改）**

| 具体文件 | 计划用途 |
| --- | --- |
| `src/core/programs/query/paths/tests.rs`、`src/core/programs/query/inventory/tests.rs`、`src/core/programs/directory/tests.rs` | 扩类型化状态、语言/环境区分、显式目标、scope及错误保真；同输入多输出不重复全量获取。 |
| `src/core/programs/install/selection/tests.rs`、`src/core/programs/shared/commit/tests.rs` | 查询后输入/对象变化、选择竞争、引用提交失败保留；保持真实锁序。 |
| `src/core/programs/install/request/tests.rs`、`src/core/programs/install/context/tests.rs`、`src/core/programs/install/apply/tests.rs`、`src/core/programs/python/integration_tests.rs`、`tests/r01_program_contract.rs` | 合同与实际facade对应；迁移重复apply断言，保留runtime pin、来源/预算、全部根、S及无声明写入。 |
| `runtimes/src/shims/mod.rs`中的测试、相应CLI文件内测试 | provider归属、wrapper/not-found/补全和初始化策略；旧写拒绝用真实入口验证，不能只测纯分类器。 |
| 现有`tests/e2e/cli/test_f1_directory_selection`、`test_f1_command_lookup_no_install`、`test_f4_python_queries_are_readonly` | 扩空库、错误/歧义、有旧Python且自动安装打开、目录切换和真实只读断言。 |
| 现有`tests/e2e/cli/test_python_bytecode_recovery`、`src/core/libraries/language/recovery/tests.rs` | 仅回归既定精确恢复及隔离真实归档，不扩大恢复范围，不操作用户库；配合CLI既有分类测试确保早分流不被新准入误禁。 |
| 现有`tests/e2e/cli/test_where`、`test_which`、`test_which_tool_not_installed`、`test_bin_paths` | 保留既有非Python/输出语义，新增共享Python分支使用专门fixture。 |
| 拟新建`tests/e2e/cli/test_p1_shared_python_queries`、`test_p1_python_write_admission`、`test_p1_readonly_initialization` | 分别承接跨命令一致/显式覆盖、旧入口首副作用前拒绝、真实启动维护/Tracker零副作用。 |
| 现有`tests/e2e/packages/f3_shared_wheel_fixture.py`、`f3_shared_wheel_helpers.sh` | 优先复用真实HTTP计数、wheel/console脚本、隔离库夹具，仅补P1确需的语言/图或哨兵，不伪造Ready。 |
| 现有`src/core/libraries/package/layouts/tests/file_descriptors.rs`、`src/core/libraries/package/layouts/tests/language_batch.rs`、`src/core/libraries/package/lease/tests.rs` | 保留64包、批次、完整inventory/manifest/namespace篡改断言；只增加必要测试观测，不改生产owner结构。 |
| 现有`src/crates/pakro-util/src/process_limits.rs`中的测试 | 原soft提升、hard限制、高soft不降、父进程不变的回归；P1不改变资源策略。 |
| 拟新建`tests/scripts/p1-check.sh`、`tests/scripts/p1-test-cases.txt`、`tests/scripts/p1-resource-baseline.py` | 薄runner、精确非零注册清单和只记录必要指标的基线driver；复用原E2E运行器，不另造测试框架，不硬编码本机binary哈希/代理/ISAT路径。 |

已存在的`tests/scripts/f-manual-check.sh`、F1–F4用例清单、`s-source-check.sh/s-source-test-cases.txt`继续提供历史回归索引，非本轮强制全跑。脚本中每组阶段有明确范围，长编译和行为验收按实际独立组定义，不拆同一超时场景绕过600秒限制。

### P1.3 实施步骤与交接

实施状态：原实现保留；查询组合、旧写准入、监督器及两项验收证据重新核验。各步不再视为全部勾选完成。

| 步骤 | 实施动作 | 交给下一步的明确结果 |
| --- | --- | --- |
| 1. 固定行为和观测基线 | 冻结P1.1命令合同、支持参数与前置判定；记录当前CLI调用/第一副作用、共享图及资源基线，选定非零测试注册。 | 当前失败/缺口可定位；基线明确代码版本、profile、图及平台。不因预期有问题就声称实际RED。 |
| 2. 统一查询事实 | 按A接同一冻结输入、语言/环境分层结果与路径投影，先让library/current/project消费。 | `只读请求+同次观测→类型化事实`；无写权限/隐式联网；状态不再折叠为Option。 |
| 3. 接路径及目录消费者 | 按B接where/which/bin-paths/env覆盖，再hook、shim/wrapper/not-found；补ls/installs空库及混合路径。 | 各命令输出可相互解释；hook释放查询保护后经现有commit重获复核，成功才应用。 |
| 4. 收敛生产安装与准入 | 按C保留真实F/S链、处理重复apply；按D接入口和底层两层门。初始化策略与此步同时落地，不将它留到验收后。 | Install/DepsInstall支持范围保持，未迁移变更早拒绝；旧间接调用不能绕过；单context/原deadline不重置。 |
| 5. 集成与资源复核 | 执行下方行为矩阵及相关F/S回归，测同binary/图大图、FD、并发及失败清理。 | 真实通过/失败/未测证据，资源计数防回退，保留600秒边界；不将小fixture当完整ISAT验收。 |
| 6. 局部退役与交接 | 确认生产消费者和测试已迁移后退出重复正文；同步最小帮助/effect说明和文档，不批量删除模块或生成资源。 | 一个可追踪的Python生产入口图、P1验收记录与P2输入；P1通过不自动启动P2。 |

接口交接使用现有数据类型优先，以下为职责描述而非已存在API：查询只交事实/描述；安装交同次观察、完整直接根和冻结上下文；runtime适配返回真实受保护对象；目录关联应用消费观察和候选后重取保护；CLI准入消费解析后的目标/操作，不持有文件写权。重复观测/新context不得在每个入口重建请求期限。

### P1.4 验收标准

下表本机适用产品合同已由用户确认通过；自动多进程运行器禁用不构成额外门槛，既有证据和未测边界见[TESTS A01–A15](TESTS.md)。所有触及三库的场景使用owned隔离根；真实本地归档、真实公网源码和确定性资源fixture分别记录。未修改用户库、ISAT配置或实际4-pyc实例。

| ID | 实际行为 | 必须观察到的结果 |
| --- | --- | --- |
| A01 | 仅语言项目、带wheel项目和已有纯Python源码项目显式install/deps install | 真实解释器/导入/console脚本可用；语言/包身份与原source/build/output证据保留；无pip/uv委托，不写项目声明或锁。 |
| A02 | 同项目再次安装、第二项目同图、第三项目不同图 | 同身份语言不重复；同图复用，异图不污染；匹配已有内容时零额外获取，关联仍由同catalog提交。 |
| A03 | 同次配置下where/which/bin-paths/env/current/project status | where为语言payload根，which与env/hook为实际有效入口，bin目录包含对应入口；普通Python和包console脚本均验证，不要求不同命令打印同一路径。 |
| A04 | cwd为Y，显式查询X；无项目显式目标；无Python声明 | 只按目标合同处理X，绝不偷用Y；不改声明/关联；非适用与未选择清楚区分。 |
| A05 | 缺语言、语言有但缺包、缺命令、无效声明/pin、多build/layout、坏payload/catalog、超时 | 类型/诊断保真；语言Ready不冒称完整环境Ready；有旧Python可运行也不回退；单路径歧义列候选。 |
| A06 | 查询空库/未登记项目，打开auto-update/lazy/自动清理等设置，使用可计数来源和旧目录哨兵 | 无HTTP获取/凭证刷新/插件安装，无自动迁移/删除；三库、catalog、配置/锁、Tracker、保护文件前后无变化，空库零初始化。不能只靠环境禁网或禁维护掩盖路径。 |
| A07 | 显式非Python、Python/非Python混合、共享库为空或损坏、特殊ls flags与补全 | 独立非Python正常；混合无旧Python重复/隐式获取；不支持组合明确诊断，不按空结果回退；输出JSON仍可解析、字段含义保持。 |
| A08 | A→B→离开→缺失/失败项目；配置在查询后变化或commit失败 | PATH和自有字节码变量恢复；未成功提交不输出新选择，退出目录不释放持久引用；保留原观察当前性和项目→对象→catalog顺序。 |
| A09 | use/upgrade/unuse/deps add/remove/旧deps install支路/prune触及未迁移Python；所有工具的install-into | 从run_inner起，第一项获取、初始化、配置/锁/跟踪写、mkdir、provider/postinstall执行或清理之前拒绝，不能只验证handler；混合批次无半成功。除已取消的install-into外，显式非Python且不附带Python保持原能力。 |
| A10 | toolset/backend直接进入、force/postinstall/rollback/purge/system/延迟清理 | 真实生产consumer被拦，不仅枚举单测；新F/S受控安装与F4指定删除不误禁；失败暂存只清本次所有。 |
| A11 | 已有bytecode恢复分类、dry-run/quiet与隔离真实归档回归 | 原早分流和精确4项边界保持，其余inventory及项目引用不变；普通查询/安装不误入恢复，不操作用户真实实例。 |
| A12 | 同项目竞争选择、A/B共享、查询/新引用与指定删除交错 | 过期观察拒绝、旧引用保留或新提交一致，无锁顺序倒置；查询不自动恢复，不把路径描述冒充进程租约。 |
| A13 | 64真实包layout获取/释放、soft256/hard4096子进程及既有manifest/namespace篡改 | 原容量/释放、hard不变、高soft不降、父进程不变保持；重复操作无FD累积，完整inventory次数不因多输出字段重复增长，篡改仍拒绝。不是hard256支持或常数FD声明。 |
| A14 | 固定大图、冷进程/重复进程、只读选择与hook应用分别量测 | 记录总耗时、观察/匹配/核验/提交分段、包数/文件数/字节、FD初始/峰值/释放、inventory次数及请求数；与同配置基线对照，新增扫描/资源增长须解释并消除非必要重复。 |
| A15 | 相关F1–F4/S回归和CLI注册/非测试构建 | 用例实际执行非零，区分ignored/helper/编译成功；已完成源码/native/字节码/删除不退化；未覆盖平台和完整ISAT自动组继续单列。 |

A14首先交付可复现基线，不承诺未经测量的固定秒数或新复杂度。历史32.71秒只是本机一次60包只读选择；P1不能以提高300秒/4096、降低hash/namespace校验或跨操作缓存换取通过。生产完整性检查保持，计时/计数仅使用测试观察或无敏感数据的已有输出。

### P1.5 验证执行与退出条件

代码实施获授权后，按实际变化选择最小有意义检查：纯合同/选择测试→真实CLI副作用与输出→相关F/S集成→资源/并发。先列出具体注册名再执行，0 tests/全部ignored/假fixture不计通过；文档中的拟新增脚本在实际创建注册前不是可运行命令。每组保持共同600秒墙钟、完整日志与同一target/profile，不通过拆分/重试绕过超时；超时如实保留未完成。

P1完成要求A01–A15中适用的必需行为均有实际通过证据；失败或未测不计完成，非适用项注明原因。已支持主线不回退，真实CLI无隐式Python副作用，重复编排已收敛或有明确唯一委托，资源基线可复现。尚未支持的声明变更、Locked包恢复与prune给准确能力诊断，不因此勾成已实现。不得以全仓编译成功替代这些行为，也不无故重跑未变化的所有历史用例。

P1实际交付：FILES消费者表、TESTS验收日志索引、资源基线及前后对照JSON/driver。查询只交描述，apply按原项目→对象→catalog重获复核；P2可沿python生产门面接统一锁及严格恢复。**本阶段按“P1本机macOS arm64产品验收通过、自动多进程运行器禁用”收尾并提交Git**；长测试由用户终端执行。完整ISAT自动组/业务、未测平台与外部进程生命周期仍后置，不能据此视为Python整体完成，不自动开始P2。

## 历史F1–F4与S实施记录（已截止，仅供追溯）

以下保留2026-10-20至24的实施、授权及过程事实；其中“当前/最新/下一步”只适用于当时。新任务以本文前部P1–P9为准，历史固定TDD/双轴流程不再强制，不将旧暂停、后移或提交授权重新应用到今天。

### S：Python源码包构建与安装（本轮实现与ISAT启动验收已截止）

**2026-10-24截止：用户已确认ISAT在Pakro环境中成功运行，并要求提交Git。** 实际24项直接依赖已安装选择，项目登记revision3、关联60个包、使用原Python3.11.17+20261013；本轮以安装→环境选用→ISAT基础启动的用户手动验收通过收尾。

1. ☑ S1 受控源码及worker（本机无子进程后端；编译器尚未开放）：实际持有/摘要校验的sdist有界解包，读取已有build-system解析；无pyproject走setuptools.build_meta:__legacy__。复用CmdLineRunner bounded child-tree及现有OS沙箱，固定共享LanguageLease/构建wheel、仅私有工作区可写，禁止联网/依赖管理器委托；真实hook、输出/路径/期限/清理十一项通过。
2. ☑ S2 构建依赖闭合及求解续传：初始/动态requires交原registry solver，保留精确输入、递归构建和环/累计预算；prepare_metadata可选回退build_wheel，最终metadata比较。恢复原depot而非伪造空依赖或重新追加根。真实nonce源码依赖+非空dynamic helper复用与不同父目录锁已实际通过。
3. ☑ S3 原源码与输出双重证据：Built owner保留原sdist pin、固定构建输入/recipe和真实输出wheel；沿原wheel检查、真实graph、共享package/layout及同catalog提交。PackageGraphNode不得把本地产物伪装registry wheel；codec有界兼容旧wheel记录，输出与构建输入参与实例区分。原3.11+iopath/ANTLR实际CLI发布、导入和跨项目复用通过173.9秒。
4. ☑ S4 真实ISAT安装、选择及基础启动（用户手动验收）：用户重试已将实际项目完整依赖选择提交至revision3/60包；EMFILE及选择5秒超时修复后，只读环境查询32.71秒选中共享layout，用户按解释器guard启动命令确认“成功运行了”。此勾选仅指该实际使用目标；先前600秒停止的自动完整组仍未完成，不记作通过。

**截止后单列的未覆盖项：** 完整自动冷组、完整ISAT图的重复/跨项目零获取验证、NMS/标注导出等全套自动业务断言，以及模型推理均未由本次用户反馈证明；C/C++/Rust编译器源码后端仍未开放。源码core跨项目复用的173.9秒既有证据保留，但不替代完整图验收。约33秒完整选择开销、FD锚共享等进一步性能优化留待后续明确任务，本轮不继续推进。

**提交收尾：** 仅纳入本轮相关源码、回归和开发记录；Cargo发行清单补齐实际include_bytes依赖的hook.py并以cargo package --list验证。禁止编译产物、缓存、发行归档和测试临时文件入Git；不推送。

#### 本轮过程记录（保留追溯，截止状态以上为准）

2026-10-24环境选择现场修复：用户完整依赖已提交revision3/60包，但hook-env --force报store deadline，guard阻止旧mise Python启动。具体接缝限定`programs/directory.rs`的两入口一次冻结总期限、`libraries/package/lease.rs`及新`lease/tests.rs`、源阶段清单/脚本必要注册。原5秒对全包图字节校验不足；改为统一有界300秒总预算（正常慢路径亦覆盖，不在中途重置，原same-env CLI快路保留）。不新增跨操作hash缓存或削弱普通read API。Object full前后保存完整store/namespace/Ready manifest核对，walk只借私有held view保持FD/SH/lock/allocation/owner和完整member no-follow/SHA；局部layout pin阶段收束同一held批次早期扫描，所有内容在组末完整校验后才能交出可用lease，独立包获取保持full，不跨query释放→project锁重获边界。先实际只读env失败及确定性计数/篡改RED，再实现与相邻GREEN；实际选择只用env只读查询，用户仍手动hook/启动，不代修改实际库/关联。

环境选择修复已当次GREEN：新5项实际RED3失败/2保护后修复，包库44通过/1辅助ignored、directory3通过，main all-features成功，整组409.5秒；同一真实ISAT只读env probe从5.02秒deadline失败变为32.71秒选中Pakro共享layout、无超时警告。完整字节/锁/manifest/namespace保护保留且补齐哈希后manifest核验；不声称选择变为瞬时或GUI已通过。注册/格式/脚本/diff检查通过，全部owned根清理，交用户手动重新hook与启动，不重装依赖。

2026-10-24真实ISAT手动安装新增EMFILE反馈：用户终端soft NOFILE256/hard unlimited，Agent宿主1048575，原自动验收遗漏低soft上限。每包pending/Ready各约17个真实保护FD、canonical重取有两套SH峰值，普通wheel原档则每枚仅1FD且物化后已释放。最小修复接缝限定`src/main.rs`、`pakro-util/src/{lib.rs,process_limits.rs}`新进程资源初始化及`package/layouts/tests.rs`/新`tests/file_descriptors.rs`、本阶段清单与`s-source-check.sh`的utility注册。仅本进程在建Tokio/加载大包图前将soft非递减提升至min(hard,4096)，保留hard、不改shell/系统，不去掉保护或改锁owner结构；硬上限不足时不得越权提高，不宣称支持强制hard256的64包图。先stub+子进程soft256真实64小wheel全布局及资源决策实际RED，再实现、验证hard不变/高soft不降/父进程不变/Drop后FD恢复，编译后交用户手动重试实际安装。原锚共享/整套重取优化作为进一步资源效率问题记录，本次不进行大范围锁模型变更或自动再次完整ISAT冷组。

EMFILE修复现已完成当次验证：资源决策4项、64真实包布局/重取/释放1项、原批次5项共10通过，all-features主程序成功，整组179.9秒；实际CLI从soft256/hard unlimited得到4096/unlimited，父进程不变，固定低hard/高soft保护也通过。最后注册/格式/脚本/diff检查通过，全部owned根清理。实际ISAT仍由用户手动重试，不能将这次隔离FD回归称ISAT运行通过。

最新真实项目证据：用户重试后，Agent仅只读project status（1.5秒exit0）确认ISAT登记revision3、24项声明、选中60个包并绑定原Python3.11.17 ready，完整依赖选择已实际提交。未取得用户安装命令exit码，不把查询成功冒充该exit；尚无本次完整环境的实际ISAT启动、全native导入、重复/跨项目复用结果。下一步由用户force hook、确认Pakro解释器后`python main.py`，Agent不代启动。

2026-10-24最新用户选择手动启动验收，明确授权先补齐真实`/Users/fuxinao/Projects/ISAT/pakro.toml`。现已按原mise24固定直接依赖补齐，保留Python3.11.17+20261013及同build packages作用域；TOML解析、与原pins逐项一致及requirements规范名完整覆盖均通过，独立只读复核无遗漏。此授权仅改变实际声明，不由Agent安装/启动或修改旧环境；间接依赖由安装时求解。后续由用户手动执行安装、确认选用Pakro解释器并启动，S4仍待结果，不继续自动完整组或性能调查。

用户已明确授权实现、Agent自行验收和每组600秒时限，沿主架构5.5的原生PEP517流程，不委托pip/uv。旧F1–F4源码构建后移约束仅对本轮registry sdist解除；其余后移功能不启动。复用同一target、关闭incremental/DEV/TEST_DEBUG，不重复构建profile；实际ISAT配置/旧环境保留，验收在owned临时配置和源码副本完成。

具体文件接缝：`src/core/programs/install/{mod.rs,resolve.rs,build.rs,build/*}`与`programs/python.rs`协调；新增`packages/src/python_source_build.rs`及`python_source_build/*`作为持源码/私有workspace/真实执行产物owner，注册于`packages/src/mod.rs`或原packages父模块；`python_resolution/source/{registry_depot.rs,registry_depot/closed.rs,built.rs}`原续传及保留证据；`python_environment/{model.rs,model/codec/graph.rs}`及`libraries/project/package_graph.rs`保存原source/输出/构建事实；必要lock_model的sdist文件名协议和既有构造测试同步。沙箱必要接缝仅`pakro-util/src/{sandbox/*,cmd.rs,cmd/*}`，worker显式严格配置默认行为保持。测试注册与CLI验收使用现有tests/scripts和tests/e2e；新具体路径在实施时续记。

现状（2026-10-24截止）：S1–S3本机纯Python registry源码构建链路已实现，S4真实ISAT安装、选择和基础启动已由用户手动验收。本轮提交并停止推进；原600秒自动组未完成、完整业务/完整图复用未覆盖和编译器源码后端限制继续如上单列，不因用户成功启动而扩称全部生态完成。

S阶段具体测试接缝：`packages/src/python_source_build/{archive.rs,worker.rs,hook.py,tests.rs}`、`python_environment/model/{source_build.rs,source_build_tests.rs,codec/graph/build.rs}`及原来源source_build测试；`tests/e2e/packages/{source_build_acceptance.py,test_native_source_isat,test_native_source_isat_full}`注册隔离验收。必要F3原生相对loader邻接回归新增`src/core/libraries/package/layouts/tests/native_link.rs`并在原tests.rs注册；待实际RED再更新该处生产布局。当前macOS严格worker还未覆盖带编译器子进程的源码包，不以ISAT纯Python通过冒称全部源码生态覆盖。

已实际确认F3 native adjacency RED（`s2-native-link-behavior-20261022.log`，32.7秒整组；S2九项及ANTLR一项通过，native一项dyld相邻路径缺失）。必要修复接缝明确追加：`libraries/package/{layouts.rs,manifest.rs,lease.rs}`及`layouts/projection.rs`，`packages/src/python_environment/prefix.rs`与必要`prefix/clone.rs`/原prefix测试。仅为跨wheel原生namespace制作独立inode的COW投影，保留原immutable包树与原manifest/lease/CAS；不使用hardlink，不借任意symlink逃逸，不静默退回整树物理复制。无法支持COW时明确报错，纯wheel布局保持轻量；布局配方变更必须进入key并验证映射来源和实际inventory。

必要复用与读取接缝追加：`libraries/package/tree.rs`仅将原storage_with_inventory扩大为同父模块可见，消费同一验证receipt；`src/core/artifact_snapshot.rs`、`packages/src/python_wheel/{zip_directory.rs,member_session.rs}`及`python_wheel_source.rs`只增加consuming真实原字节复制/重新验证产物的接口，供新增`python_source_build/cache.rs`保存本次operation首次实际构建的sealed owner。缓存以原pin、完整语言/target、固定recipe/settings作用域区分，保留BuildRequestId、实际输出及输入pin；不把首次input描述冒充仍持有原临时输入锁，也不要求同源码重新执行得到同digest。输出路径必须从held workspace FD逐组件NOFOLLOW读取，不依赖backend沙箱对rename的隐含假设；PEP517辅助metadata文件保存但不强制出现在最终wheel。

最终注册与功能回归：`tests/scripts/{s-source-check.sh,s-source-test-cases.txt}`逐filter确认非零再运行，core/full ISAT为分开的整组冷验收；`src/core/programs/install/build/tests.rs`加真实nonce源码构建依赖、非空dynamic新增helper的coordinator测试，其真实HTTP/source fixture扩在原`registry_depot/tests/project_fixture.rs`，验证首次实际产物贯穿动态求解，不能靠DTO或假wheel跳过执行。

二进制编译必要兼容接缝：`cli/src/commands/{exec.rs,run.rs}`原SandboxConfig完整literal须同步填写新增worker专用字段`build_worker=false/allow_process=[]`，与原`task_executor.rs`相同，仅保留默认旧任务策略。主程序实际编译先报E0063后修此两处，不恢复旧exec或任务功能。

S4真实完整依赖额外接缝：首次full24冷验收49.2秒在原Simple facts的filename-version与顶层versions交叉要求失败，尚未进入完整安装，不能称通过。对公开24份Simple JSON使用原parser显式ignored probe确认真实数据及最小兼容行为；接缝只限`pakro-index/src/simple/{facts.rs,json_tests.rs}`及原文件身份验证回归，不能因此跳过下载摘要、文件名目标或最终METADATA一致性，也不更换源或旧环境代装。先保存实际RED再决定生产处理。

S4 index最小兼容处理：原parser actual probe已证明numpy/PyQt5/regex旧文件与顶层versions不一致（4.6秒有效RED）。顶层release声明原样保留，不把它强行等同每个filename-derived版本；新增`pakro-index/src/simple/mod.rs`静态warning枚举记录缺少对应声明，不替换文件版本、不降格Historical格式、不伪造归属。保留versions字段存在、集合重复/空元素和所有预算规则；真实选中artifact依然凭其filename、原registry pin、hash、target和实际METADATA验证。旧cross-field拒绝回归改验证warning及保留file identity，原文件拒绝测试继续保留。

S4有界候选成本接缝：索引修复后第二次full24在75.8秒触发原registry budget，尚无完整安装。新增`python_resolution/candidate_policy/tests.rs`显式actual descriptor+24pages纯probe确认到底是何项限制；只读actual language manifest，不执行用户Python。必要优化范围限定`candidate_policy.rs`/新同目录缓存或匹配子模块、`source/registry_depot.rs`与`registry_depot/active_policy.rs`和原对应tests；如初次rank检索确需优化，额外限定`pakro-index/src/wheel.rs`及其原测试，保持tags顺序/结果/作用域和现有总体资源上限。先有真实预算RED及测量，不凭猜测抬limit、丢不兼容项之外的候选或重置Usage。

S4已测量后的具体rank回归路径为`pakro-index/tests/wheel_rank.rs`：压缩标签小矩阵对原有ordered scan oracle等价、大压缩集合选择bounded fallback、同原rank且成本界限、非法标签/重复目标原拒绝保持。纯诊断descriptor快照在target/s-isat-language-observation-20261023.json（来源manifest SHA记录，无运行时权限），后续probe用快照不反复读取实际用户库。

S4分类缓存具体模块：`python_resolution/candidate_policy/cached.rs`的opaque owned静态描述，只保存原file_index/raw version/解析版本/静态兼容与yanked事实，放原ProjectEntry并由原source owner作用域配对；不缓存跨metadata/constraints的Allow/Reject。新cached active与旧pure kernel对3-state knowledge/constraints/预发行/yanked/===做结果和notices等价校验，继续累计Usage，新增活跃约束成本相邻边界测试。注册于原candidate_policy.rs，辅助原test producer只收窄可见性供同父测试复用。

S4回溯修复接缝（2026-10-23用户续接）：先修自有watchdog的超时最终清理并做短进程自检；产品改动限定`packages/crates/pakro-resolver/src/python_native/provider/active_policy.rs`和`python_native/policy/tests.rs`，必要真实请求回归复用`packages/src/python_resolution/source/registry_depot/tests/active_policy.rs`。当前策略Reject后按整个闭合图字母序生成nogood，首先更换无关ANTLR/AnyIO；先用同版本wheel/未知sdist与独立被拒候选离线复现。保留完整nogood的逻辑和预算，只优先重试真正被拒的包，不能全局禁用预发行或删去可能引入新约束的其他图节点。先实际RED再实现，父约束变更可合法化预发行的既有测试继续通过；修复验证后再整组隔离full24 ≤600秒。测试注册同步原`tests/scripts/{s-source-check.sh,s-source-test-cases.txt}`加入resolver policy非零清单与执行。

S4等待定位续接（2026-10-23用户再次授权）：继续每组600秒。先只改`tests/e2e/packages/source_build_acceptance.py`增加显式`--trace-resolution`诊断选项，在最终私有env启用现成PubGrub INFO并给输出单调计时；保留原所有验收断言/空环境，不启用HTTP debug、日志只限有界文本。用原已编译binary重跑一整组带诊断的full24定位停点，不先改产品语义或抬预算。若需要进一步registry项目/metadata边界计时，接缝限原`python_resolution/source/registry_depot.rs`及其`active_policy.rs`/tests，实施前记录实际观察和具体修复合同。

等待取证已确认的UI修复接缝：`src/core/programs/install/build.rs`在实际源码产物已验证/保留后、`continuation.resume_with_built(...).await`前，将进度改成恢复依赖求解。trace第153.79秒已选OpenCV而iopath仍显示build-and-verify，属于真实提示失真；只改阶段文字，不改下载顺序、来源、候选、验证或预算。当前诊断整组继续使用已编译binary，结束后再单独有界编译刷新，避免编译IO影响耗时测量。

短对照接缝：同代理同Torch原地址8MiB Range，curl H1为3.08秒、H2为31.73秒，本地写0.005秒。先在`pakro-index/src/simple/client/file/tests.rs`增加显式ignored `s4_registry_http_protocol_probe`，只读指定公开Simple捕获中的固定Torch文件，比较原SimpleIndexClient真实builder（root同features）与受控HTTP1 builder；每个请求45秒、仅206/精确8MiB、无pin/安装发布。未证实应用协议前不全局禁HTTP2、不改窗口、不新增用户配置。诊断harness日志同时修正按实际输出含前缀计费、限制单行及有界终止等待。

实际Simple短probe已通过，但默认H2为15.144秒、H1为14.551秒，未复现curl差距；不能按curl结果改生产协议。仍仅在该ignored probe追加现有必选rustls backend控制（默认协商/H1），与Cargo feature union下默认TLS比较，排除TLS后端差异；每请求45秒/同一8MiB，不据一次速率推断通用性能。无生产协议/TLS配置修改授权推断。

等待定位当前结论：完整带trace组553.1秒主动结束并清理（非通过/timeout），确认OpenCV整wheel48.3MB及Torch127.3MB串行传输，FD持续增长；Torch局部约235.6KiB/s。四控制组一次Rustls较快，但反序Rustls H1在45秒body期限失败，未形成稳定协议/TLS因果，不改生产网络默认。源码恢复阶段提示已修改并实际编译70.5秒，最终format/diff2秒通过。已联系用户切换更稳定节点或继续查下载调度，S4仍未勾；不能继续盲跑完整冷组、把诊断退出/局部Range通过当完整ISAT验收。

**2026-10-23 22:50用户决定：** “下载速度没关系，以后再说”。速度及协议/TLS/调度性能优化后移，本轮不继续调查、不要求更换节点；现有源码构建核心、回溯与阶段提示修复保留。此决定不代表完整ISAT已验收，也不放宽600秒测试上限；S4仍未勾。

**最新续接：继续完成功能及ISAT验收，下载性能后移。** 将真实发行归档作为测试输入独立准备，原完整24 pin/原生库/Qt/应用与源码构建断言在全新owned data/cache中一次执行≤600秒；不跨组保存solver或安装状态，不称公网全量冷装已通过。使用既有FrozenRouter的私有URL_REPLACEMENTS，仅把公开无凭证pypi/files.pythonhosted原逻辑地址路由到loopback fixture；保留原Simple JSON字节、候选、hash、Requires-Python、yanked、完整wheel/RECORD和sdist构建验证。

具体测试接缝：新增`tests/e2e/packages/isat_release_fixture.py`，只读预先校验的原发行归档和原索引页，缺失输入立即失败，无外网补货/空依赖/旧安装树代用；原`source_build_acceptance.py`加显式`--release-fixture`路径与实际ISAT Annotation保存重载、LabelMe/VOC导出及torchvision NMS小样本。测试输入在ignored `target/s-isat-release-inputs-20261023`，manifest含原page/artifact SHA及大小，完整raw字节须与公开Simple原hash匹配；旧环境METADATA只能提供准备候选提示，不能作闭合图/安装证据。单独的输入准备每组≤600秒，已验证真实归档保留作后续输入，partial清理；功能组始终从零且所有验收断言保持，首次真实网络source-core173.9秒证据保留。

完整功能暴露的候选重试缺口：第二组110.3秒请求hf_xet1.8dev0同版本sdist；原hub metadata无显式pre约束，pre/stable wheels均RP>=3.8、仅非活跃pytest extra，已知final可满足策略。原Reject只排当前file，随即试同版本pre源码而不先使用已观测Eligible final。修复接缝限定`pakro-resolver/src/python_native/{policy.rs,provider.rs,provider/active_policy.rs,policy/tests.rs}`、`python_resolution/candidate_policy.rs`/`candidate_policy/{cached.rs,tests.rs}`、`source/registry_depot/active_policy.rs`及原真实peer tests。新增显式RejectWithAlternative建议，由app同次约束/knowledge推导合法已观测final并映射原file_index到key；core只验证并在紧接retry的range内优先，不硬pin、不删候选、不合并同版metadata、不缓存跨图Allow/Reject，原wholeclosure nogood/预算保持。先API/stub及用例实际RED，再实现；覆盖建议失败回退、父分支合法化原pre、未知/foreign/rejected建议拒绝、原index映射和真实peer零source请求。

多项目实际回归修正建议寿命：单轮版本142项通过但完整图119.2秒仍取hf pre source；新增两个独立pre项目实际RED证明修第二个项目时清掉第一个排序建议。建议改在同一immutable provider输入快照内按name保存有界BTreeMap，generic Reject撤销当前name、withAlternative替换当前name，其他项目排序提示保留；NeedInput/结束provider Drop全部丢弃。仍每轮完整policy复核、range检查和全部原候选，不缓存Allow/Reject/元数据/跨depot结论，旧hint不是selection证据。同步API文档和先前“只活下一轮”实现细节，以multi-project RED及原回退/父分支/预算用例验证。

2026-10-24用户续接授权修复重复语言核验并完成ISAT验收，仍每组600秒。精确接缝限定`libraries/language/lease.rs`、`libraries/package/{tree.rs,layouts.rs}`及`layouts/tests.rs`/新`layouts/tests/language_batch.rs`，测试清单同步`s-source-test-cases.txt`。同一次layout纯wheel物化批次使用借用同一LanguageLease的私有scope：批次开始完整inventory核验，逐包仍核held SH/lock/namespace/manifest当前性，批次结束在首次pending发布前完整核验，原layout末尾full保留。独立tree入口前后完整核验不变；无跨operation hash缓存、mtime替代、可伪造DTO或通用cheap check权限。先用实际inventory walk计数和N=1/N=3完整layout行为证明RED，再实现；同inode同size语言变化、manifest/namespace更换均须拒绝且不发布任何pending，保留原package output核验及失败清理。编译与完整原发行输入ISAT仍分开有界组，功能组从零执行所有原断言。

**2026-10-22 19:42用户后续验收：** 库存与project status确认ISAT revision2关联同一Python及PyYAML实例；用户离开ISAT恢复mise shim，重入后自动导入共享yaml6.0.3。基本声明→安装→导入→关联查询→目录选择已实际通过，不重复测试。当前终端接入已启用，本任务未修改.zshrc；完整ISAT仍缺其余依赖及源码包安装能力，后续范围需用户指示，不自行扩展当前F1–F4。

**2026-10-22 19:30用户现场验收：** 用户自行deps install成功29.41秒，force hook后普通Python确认为原共享language实例，PyYAML6.0.3从共享packages路径导入。实际Python+PyYAML安装/选择/导入已验收；下一步教程为installs ls --json及project status只读查看实例/项目关联。完整ISAT其余依赖及sdist范围限制保持，不将此结果称应用环境就绪；无新代码或构建。

**2026-10-22 14:41当前状态：** 用户已成功恢复实际4项pyc；本轮F3代理/期限接线、7阶段安全错误和多Generator兼容已实现、编译并验收。原来源50项、客户端45项、真实代理CLI通过；wheel检查50通过/1显式ignored，真实PyYAML发行probe另行1通过。最终同一226.7秒组在隔离原3.11.17+20261013安装PyYAML6.0.3、普通导入/CLoader/safe_load及坏代理下再次复用均成功，语言manifest与项目声明不变。下一步交用户在ISAT手动重试deps install，不代其安装。每组低于600秒，原共用target/关闭incremental/debug配置，临时树已清，未提交Git。完整ISAT的sdist范围限制仍保留。

**PyYAML parser具体接缝：** 原发行wheel probe有效RED为`WHEEL duplicate Generator`。官方wheel由setuptools及delocate写入两个构建工具信息；当前规范未明确唯一，原代码也不以Generator作任何安装判定。只在`packages/src/python_wheel/metadata.rs`移除这个描述字段的额外唯一限制；`packages/src/python_wheel/tests.rs`保留显式真实发行验证并新增小型正常/关键单值与RECORD拒绝回归。其余字段、身份、Tag/Build、预算、文件/RECORD和摘要规则原样执行，不新增来源或源码构建。

**2026-10-22 PyYAML真实wheel回归（已完成）：** 初次隔离3.11+实际PyYAML6.0.3在archive检查失败，经显式本地已校验发行wheel probe确认Generator误拒绝，按上方具体接缝修复并完成最终实际安装/导入/复用。所有阶段保留文件、RECORD、身份和目标兼容校验；下一步用户自己重试命令。

**2026-10-22 F3现场接续：** 用户已自行预览并成功恢复4个pyc、核对其余2240项。随后`deps install python`复用语言成功，但5.46秒在registry获取/metadata合并错误处失败。只读审查确认应用以`SimpleHttpConfig::default()`创建客户端，默认显式禁用代理，并未传入已冻结的HTTP期限；底层6处错误映射丢弃安全类型原因。本次仅修复现有F3接线：`src/core/programs/python.rs`传入配置；`packages/crates/pakro-index/src/simple/client.rs`增加默认关闭的环境/系统代理显式选择及可选读取期限，保留构造时快照与默认禁用边界；`packages/src/python_resolution/source/registry_depot.rs`及其测试保留原安全类型阶段原因。测试接缝限定原client测试、`tests/e2e/packages/f3_shared_wheel_fixture.py`和新增`test_native_deps_proxy`，登记至本阶段测试清单与脚本。先实际RED再实现；每组600秒、共用target，无实际用户安装/配置/库修改，不切源或跳过摘要校验。

**2026-10-22恢复验收完成：** 20项恢复含在71项核心/相邻检查GREEN；store58项、install_state18项、bin classifier2项通过；真实3.11原归档隔离E2E通过131.3秒整组（含CLI编译），验证只恢复4个pyc、2240项及项目引用不变、预览/quiet无修改、错误拒绝、重复noop及正常启动复用。用户随后已自行预览并实际恢复成功（Restored 4/Verified 2240）。测试注册、格式/语法/diff检查通过；全部组低于600秒，当时target约6.9GiB，完成组临时根已清，未提交Git。此前状态/证据保持历史，最新交接见HANDOFF。

**2026-10-21现场状态：** F1普通启动改写共享pyc的预防接线修复已通过真实隔离CLI、配置覆盖及底层回归；用户真实3.11对象已失配，本次4项恢复已获用户明确授权，正在实施隔离验证；其余恢复仍受下文后移边界限制，ISAT包安装尚未成功。具体文件与证据见“F1现场回归”及最新HANDOFF，不把预防修复冒称现场恢复完成。

**2026-10-20 当前授权：** 用户要求继续完成 F1–F4，Agent 自行编译、测试和检查；每项测试/命令组墙钟上限 600 秒，超时停止本组及自己启动的子进程并联系用户，不能靠拆分重跑绕过。复用同一 target 和关闭增量/调试信息的构建配置，及时清理无用编译产物，保留日志与真实输入。该指令覆盖下文历史手动执行限制，架构及阶段范围不变；不自动提交 Git。

**本轮具体接缝补充：** 新增 `src/core/programs/library.rs` 作为 F4 独立 CLI 可调用的只读查询/指定删除门面，不公开 IO owner；F3 生产入口新增 `src/core/programs/python.rs`，仅包装已有观测、语言、求解、布局、单 catalog 提交。F3 必要新增接缝 `packages/src/python_environment/prefix.rs` 以消费原受控未发布 owner 并持久化共享发布，仍保留原 cleanup 权限限制。F3 必要协议接缝 `packages/src/python_resolution/source/registry_depot/closed.rs` 消费已闭合图为 wheel owner 和持续证据，复用原保留核，退出完整runtime复制。F2 按原文件清单补 `install_rollback.rs`、`system_install.rs` 的共享对象拒绝门及真实入口测试。

**当前状态（2026-10-20）：F1–F4已完成本机macOS arm64首版实现与本轮授权的Agent验收。64项不同核心用例、8条CLI验收及实际公开PyPI安装均通过；底层及相邻回归通过。F2官方完整下载/运行/双项目共享/重装复用经现有SOCKS接入通过300.9秒。** 所有组均少于600秒；用户已明确授权提交本轮改动，提交号以git log -1为准，不包含编译产物或测试临时文件。

架构仍以[主文件](../../Pakro主架构文件与Agent规则.md)为准。原R00–R45详案完整保存在[历史详案](archive/PLAN-R00-R45-2026-10-18.md)，用于追溯；它不再是必须逐项完成的前置队列，也不恢复历史测试授权。本轮实施权限来自上方用户授权，计划本身不额外授予首版以外的实施权限。

**本次细化方式：** 恢复原详案的逐文件清单、接口交接、实施顺序、边界和验收合同，仍只有F1–F4四个交付阶段。下文行号为2026-10-18工作树的定位锚点，实施前按函数名重新定位；包含已有未提交/未跟踪实现，不以HEAD替代当前文件。

### 为什么缩减

本轮必要共享接缝追加：`packages/src/python_resolution/lock_session.rs` 的原解码/同文件比较纯核供已观测锁pin复核；`packages/src/python_environment/model/codec/graph.rs` 复用有界codec保存完整图及来源pin；`runtimes/src/plugins/core/python/native/transport.rs` 的无日志纯路由供公开Simple来源使用；`native/pipeline.rs` 与 `libraries/language/lease.rs` 提取同一真实目标身份。均服务F1–F4现有合同，不增加产品功能。

验证与来源具体文件追加：`src/core/libraries/project/runtime_pin.rs` 在已观测字节上读取运行时pin；`src/core/lockfile/mod.rs` 从原read提取同一纯parser。测试在 `programs/python/integration_tests.rs`、`programs/shared/commit/tests.rs`、`libraries/project/package_graph/tests.rs`；真实CLI复用 `tests/e2e/packages/f3_shared_wheel_fixture.py`、`f3_shared_wheel_helpers.sh` 与本阶段表列三个E2E；`registry_depot/tests/project_fixture.rs` 只增加原夹具producer。上述新增模块按父mod注册，不扩大正式写域。

F1真实消费者补充：`cli/src/commands/shim.rs` 是原shim执行入口，与已列 `hook_not_found.rs`、`runtimes/src/shims/mod.rs` 一同接共享Python只读命令选择，关闭not-found/lazy隐式补装；复用原入口不新增命令。增加 `tests/e2e/cli/test_f1_command_lookup_no_install`，开启旧自动安装配置仍必须零获取，已有命令真实执行。

F2真实联网接缝补充：`runtimes/src/plugins/core/python/native/catalog.rs` 在原解析/来源/校验/缓存核上增加精确请求scope，`python.rs` 在catalog获取前保留本次完整版本/build。已指定build只查询对应官方tag及所选制品摘要，未指定build仍真实证明唯一或列出至少两个实际候选拒绝，不能选择最新。精确cache不得冒充旧完整版本清单。此改接消除精确安装对无关历史发行和其checksum的依赖，不改变指定来源、版本及原期限。

最终完整性复核接缝：`src/core/libraries/project/runtime_pin.rs` 与 `runtimes/src/plugins/core/python/native/pipeline.rs` 复用原纯 locked-source/filename 校验，保留锁条目自身 `+build` 约束，不能因本次裸版本请求丢失。`packages/src/python_environment/model.rs`、`src/core/programs/install/resolve.rs` 将原闭合图 fingerprint 正文提为唯一纯核，`src/core/programs/shared/commit.rs` 与 `src/core/programs/query/snapshot.rs` 对照同一真实 layout graph；模型 codec 读取必须拒绝保持旧 graph digest 却更改 roots/依赖声明的记录。 `src/core/libraries/project/package_graph.rs` 的只读视图携带模型已验证的 graph digest，`package_graph/tests.rs` 同步其纯描述夹具；不在每次查询重复序列化整图。测试仅增补 `src/core/libraries/project/runtime_pin.rs`、`src/core/programs/install/resolve/tests.rs`，必要模型旧夹具在 `packages/src/python_environment/model/tests.rs` 改为真实 graph fingerprint；先确认 RED 再实施，不新增来源、解析器或 IO 权限。

旧计划的问题主要是交付顺序，而不是所有检查都无用：

- 普通install被R07/R10–R15的完整横向任务挡住；用户很晚才能用到已完成的共享内核。
- 目录选择被放进完整查询接线，实际最先需要的只是配置匹配、已有Python路径和缺项提示。
- 首次安装和选用，本不必等待完整升级、物理回收、多文件恢复以及所有源码来源。
- 已有文件/制品/锁内核应直接复用，不在每个消费者重新建一套防护或重复跑未改变的全套测试。

新的实施单位是“用户能完成一件事”，而不是“又完成一个内部模块”。四个阶段分别交付选择、安装、包使用、查询与指定删除；不把原详案全部工作藏进几个标题，首版之后的能力仍明确后移。

### 第一版边界

- 先验收用户当前的macOS arm64机器和官方PBS预编译CPython；Linux/PyPy完整矩阵放到后续验证，不声称首版已经覆盖。
- 用户沿用已有项目配置，不自动编辑声明；项目成功选用的语言/包及依赖关联必须有可复核的持久记录，供F4查询与删除检查。纯查询不登记项目，内部关联维护不增加用户操作步骤。
- 包阶段先一个公开Simple来源、已有兼容wheel的普通Python包，优先用纯Python包验证；真实传递依赖和兼容检查不能跳过。
- 目录内已有就选用；缺项由用户决定安装；安装没网明确报错；没有独立离线模式。
- 允许必要内部路径/启动适配，不要求用户创建、命名或逐项目激活环境；不委托pip/uv完成本应原生完成的安装。
- F4开放明确指定的语言/包删除：有项目或依赖引用就列出并拒绝，确认无引用才可删除。自动批量prune、强制覆盖、原位修复及自定义postinstall仍后移；旧无许可删写旁路须在副作用前拒绝。
- 原有其他功能不因此被删除；只说明这一条新共享路径的首版支持范围。Python→Java→Node的长期顺序和既定最终能力仍保留。

### 贯穿F1–F4：完整精确版本与真实关联

- 用户下载请求及项目直接清单必须写完整精确发行版本。Python例如`3.11.14`；`3.11`、`3`、`3.11.xx`、通配、范围、`latest`/浮动别名均不得自动补全。`xx`只是用户举例的占位，实际填写时须是真实补丁号。
- 安装命令可读取配置中已明确的完整版本，但参数和配置都没有完整版本就报错；拒绝发生在远端获取/下载及业务写入之前。包按其生态的真实完整发行号表达，不随意补零造版本；查询可列候选，但不会自动把候选变成下载目标。
- 包作者metadata中的传递依赖范围仍要真实解析；每项实际下载前必须确定精确版本/制品并记录，不要求用户手填全部间接依赖。完整版本不替代平台、发行build、来源和摘要校验；同版本多实例有歧义时列出候选，不任取或选最新。
- F1–F3逐步维护项目→精确语言/包、包→实际依赖及适配→语言的关联。退出目录只撤当前终端选择，不释放项目持久引用；新选择成功后才更新关联，失败、配置损坏或项目暂不可访问不把已有引用当作零。
- 完整可移植锁与复杂事务矩阵可以后移；实际版本和最低关联记录、其真实写入保护不能后移，否则F4不得开放删除。关联更新与查询职责分开，不让只读查询隐式写库。

### 文件落位、读写权限与执行纪律

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

- 每阶段按“行为测试并注册→实际确认失败→最小实现→复测→Spec/Standards两轴静态审查”推进；本轮用户已授权Agent自行执行，只有有实际结果才勾选，失败原因和未执行范围单列。
- 新文件须在表内父mod注册。CLI在独立binary crate，公开最小程序入口/请求/报告；库IO/owner保持私有。删除默认是**无**，只有对应旧调用确已替代且用户验收后才局部退役。
- 本轮按用户授权完成F1–F4，自行编译/测试/格式检查，每组600秒；只清理已确认归属且无用的本轮产物，不动用户库或原输入，不自动提交。首版以外工作仍须新的用户指令。

### F1：进目录就选中库里已有的 Python

**状态：** ☑ 测试先行/实际RED；☑ 实现与旁路补漏；☑ 本轮授权的Agent实测验收；☑ Spec/Standards审查。12项核心、目录CLI与命令查找CLI均通过（f-core-final、f1-cli-accepted、f1-cli-command-accepted-20261020.log）。覆盖优先级、完整版本/来源pin、缺项零获取、实际包选择、跨target旧引用退出与命令缺失不隐式安装。历史2026-10-19人工验收保持原日志，不冒充本轮人工确认。

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

**本阶段新增文件：**

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

### F2 现场回归：ISAT 精确安装准备超时（2026-10-20）

用户按教程手动安装 `3.11.17+20261013`，触发 `language preparation timed out`。只读定位：新 `programs/python.rs::capture_install_context` 固定600秒，覆盖原 `settings.http_download_timeout()`；`PythonPlugin` 显式入口强制QuietReport，使原阶段提示不可见。实际用户cache只有精确catalog元数据、无已验证artifact，语言payload为空且无Ready；不能据此伪称已确定具体网络故障。

本次只修既有合同接线：`src/core/programs/python.rs` 使用同次冻结Settings的原下载总期限（含checked_add），测试复用已有微型真实协议归档及原prepare_language核验证短配置会超时、长配置不被600秒截断、取消/失败不发布；`runtimes/src/plugins/core/python.rs` 复用原MultiProgressReport/SingleReport显示获取、摘要、来源证明、解包验证和成功/失败阶段，保留quiet，日志不输出凭证/URL。必要测试在上述cfg(test)、`tests/e2e/cli/test_f2_install_progress`及清单 `tests/scripts/f2-test-cases.txt`，复用原F1隔离暂存助手和原run_test。若还需其它协议文件，先具体补计划；不改下载/来源/摘要/租约保护核，不关闭校验，不改ISAT配置/版本，不代用户重试安装，不自动提交Git。目录选择5秒与查询期限保持。

先补失败测试→确认RED→最小实现→相邻回归/CLI小夹具验证→编译同一target。Agent每组仍以600秒外部墙钟上限运行，超过即停下联系用户；测试上限与产品按Settings取得的下载期限分开。本次修复已通过22项F2+6项原runtime回归（28/28，78.1秒含编译）、主CLI编译（68.7秒）、真实CLI小夹具（3.7秒，显示复用/失败阶段且quiet无输出）、注册与格式检查。没有替用户重试真实3.11安装；原F1–F4证据保持历史，不冒称现场真实下载已通过。

现场人工续接前提：D/languages/language-670a04e9cf8e3636a5fd4a9fcfcd15e2d2153f1b1acb7be6e0ebd66e9018244e 及其payload已只读核验为UID501、0700、非symlink，外层只有payload、内层空，Catalog无Ready。旧期限过期使原owner按既定规则保留分配，后续reserve会明确要求recovery。通用恢复仍按计划后移；不给自动覆盖/清单删改旁路。拟交用户在无并发安装时只用rmdir处理这两个已确认空目录（非空自动拒绝），保留稳定锁/注册/清单；任一错误则停下，不能扩大清理范围。

### F1 现场回归：普通 Python 启动改写共享字节码（2026-10-21）

用户真实安装并启用无包Python后运行普通`python -c`，继而追加PyYAML声明时出现`published language inventory changed`。只读全量核对确认2244项中仅4个发行归档已有pyc被正常启动重新编译（inode/size/SHA改变）；其余2240项一致，原4项bytes与已验证缓存归档吻合。安装内探针带-B，有包wrapper已有PYTHONDONTWRITEBYTECODE=1，唯独无包hook/env只接PATH漏掉同一保护。不能通过忽略pyc、放宽库存或改采现有文件清单解决。

本轮F1最小修复文件：`cli/src/commands/hook_env.rs`成功选择共享Python后向原EnvDiff输入设置PYTHONDONTWRITEBYTECODE=1，保留退出恢复；`cli/src/commands/env.rs`所有原输出分支通过同一共享路径辅助函数设置该保护，处理PATH已在首位的早返；`tests/e2e/cli/test_f1_directory_selection`补继承0被覆盖、离开/缺项恢复、shell/JSON/dotenv/values/extended一致性。必要真实PBS隔离回归可新增`tests/e2e/cli/test_f1_shared_python_bytecode`，复用原F3暂存助手、本机原始3.13.16归档及run_test，先确认普通启动后仍可复用/选择、再核对完整库存不变；测试来源/输出注册到`tests/scripts/f1-test-cases.txt`及`tests/scripts/f-manual-check.sh`。每组外部600秒、同target及debug0/incremental0；不联网、不操作用户3.11库或ISAT、不自动提交。

同因必要接缝补充：`cli/src/commands/shim.rs::which_shim/handle_shim`保留实际共享选择来源标志，随原子进程环境传递固定保护；`cli/src/commands/exec.rs::run_with_toolset`仅接收上述已经选定命令的环境，不改变普通exec的工具解析/安装行为。wrapper明确返回共享选择时同样覆盖其环境中的0，非共享shim保持原样，不按路径/空Toolset猜身份。独立真实shim回归先RED再改接；env extended输出不得被配置来源展示重新覆盖为0。

现有用户3.11 Ready已经失配，预防修复不能自动恢复。现计划明确将原位修复/强制覆盖后移；本轮先完成上述预防和隔离证据，形成恢复的具体范围后交用户裁决，不偷偷覆盖清单、删除共享目录、切换发行build或重下归档。

**2026-10-21已获授权、待实现/验证的恢复范围：** 只处理本次LanguageKey下4个已与原归档匹配的pyc。须先暂停用户直接运行，取得EX并固定旧manifest/目录身份，完整校验本机缓存归档SHA，要求其余2240项严格一致，再从原归档恢复这4项原始bytes，完整复核后用旧manifest快照CAS仅更新相应inode事实；不采纳现有新缓存、不改项目引用、不删整套目录、不重下载。任一额外损坏、并发变更或身份不符即拒绝，正式入口前须隔离验证中断/重试等行为。普通install/uninstall及旧force当前均不能安全恢复有引用且库存失配的实例，不能用手工JSON修改代替能力实现。

2026-10-21恢复实施文件：新增`src/core/libraries/language/recovery.rs`及`recovery/tests.rs`，在`language/mod.rs`注册；只从本机cache SHA快照恢复固定4项，原EX与Journals记录旧清单和stage/backup固定身份，按no-clobber捕获与安装，旧清单CAS发布并收尾。`packages/src/python_environment/prefix/verify.rs`只增加纯库存根/全集严格比较接缝，不放宽observer；`src/core/programs/library.rs`增加明确版本+实例且固定本次官方制品的应用门面；`cli/src/commands/install.rs`新增显式`--restore-bytecode <INSTANCE>`与dry-run，最早分流、拒绝混用，不安装项目包；必要接缝`cli/src/commands/mod.rs`在正常pakrorc/全局更新/registry/迁移/auto-prune之前识别并分派该显式选项，复用既有本地query的受限Settings初始化及--cd验证，不因此启动其它生命周期。新增`tests/e2e/cli/test_python_bytecode_recovery`，必要只读原归档路径经`tests/e2e/run_test`传入隔离测试，清单/runner注册在现有f1脚本。先行为RED再实现，覆盖逐阶段中断、重试、未知变化、活租约、旧快照冲突及归档不符；真实用户库/ISAT保持只读，之后交其手动执行。每组600秒同target/debug0/incremental0，不自动提交。

额外必要纯事实接缝：`packages/crates/pakro-store/src/control/mod.rs::ControlSnapshot::file_identity()`只返回该真实快照已pin住的inode描述，不发放写权，用于恢复内核在CAS后验证“当前清单确为本次CAS的inode”，拒绝同bytes换inode。CLI同因窄门拒绝inline backend opts，避免静默忽略其它路径/选项。

跨重启的已发布清单身份也必须在CAS之前可持久记录：必要接缝`packages/crates/pakro-store/src/control/write.rs::replace_from_journal`只把同store已pin的Journals元数据inode按旧Catalog快照CAS移动到Catalog，复用原snapshot校验/锁/rename/fsync，不重造新inode；行为测试位于原control/tests.rs。恢复WAL先固定预制final manifest的name/id/sha，再发布；这样CAS前后中断均能严格认领同一个清单，拒绝同bytes不同inode，不留CAS后补记录的未知窗口。先stub RED再实现。

2026-10-22续接：在初始WAL前与4个pyc stage一起预制最终manifest。`verify.rs`复用原digest算法的纯`with_file_identities`只在4个去重已发布regular文件上替换计划身份，最终仍须与真实完整observer结果全等后才CAS；不把计划描述当验证许可。真实CLI测试从原harness的临时根中另分配全空data/cache子根，避免预置plugins影响stage helper；未知flag在真实恢复选项前仍应走受限初始化并由全parser拒绝。

CLI独立binary的必要只读接缝：`cli/src/args/backend_arg.rs::has_explicit_backend`公开既有布尔事实getter（doc hidden），用于明确拒绝显式backend拼写，不发放IO/解析/写权；原pub(crate)在library与binary之间不可访问，不能用猜测full路径替代。

真实CLI验收补充的必要接缝：`runtimes/src/toolset/install_state.rs::init_readonly_plugin_metadata()`仅为ToolArg解析装载本地插件类型，跳过banned插件清理、安装扫描、迁移和脚本执行；普通init保持原逻辑。恢复早入口在解析参数前调用该只读初始化，不能调用带清理的普通init，也不能把未初始化静默当作无插件。隔离E2E保留banned gradle目录及脚本/配置hash，验证无删除/执行/改写。

预防修复本轮验收：主CLI18.0秒；3条CLI组171.0秒；项目与wrapper空env真实PBS回归84.5秒；同源缓存lib binary的51项完整性/查询/删除保护回归8.3秒（3显式ignored）；格式/shell/diff检查通过。真实普通启动不手写-B，对初始manifest和完整payload库存前后核对并能复用。实际用户3.11对象未恢复。全部组低于600秒，target约6.0GiB，临时根清理，未自动提交。

### F2：用户决定安装缺少的 Python，装好就能选用

**状态：** ☑ 测试先行/实际RED；☑ 精确安装、来源/build pin及旧写入口拒绝；☑ 本轮授权的Agent实测验收；☑ Spec/Standards审查。19项核心、原catalog26项、官方完整网络CLI均通过。真实下载Python 3.13.16+20261013、A/B同payload、重装仅一个实例、模糊请求零安装，证据target/f2-cli-socks-accepted-20261020.log（300.9秒）。HTTP代理实测native TLS -9806连接关闭，正式验收采用机器已启用的SOCKS协议；没有修改来源、摘要、版本、期限或加入生产自动重试，失败日志与网络诊断保留。

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

### F3：配置里的一个真实 Python 包能导入

**状态：** ☑ 测试先行/实际RED；☑ 原生闭包、共享包树/轻量布局、同catalog精确图与提交；☑ 本轮授权的Agent实测验收；☑ Spec/Standards审查。21个不同核心用例均已有GREEN：f3-accepted组20通过，唯一旧fixture任意摘要改为真实fingerprint后于f-core-final组通过。三个真实PBS+wheel CLI均通过（88.1/121.1/120.3秒）；实际公开PyPI idna==3.10安装、中文域名编码及查询通过（f3-live-pypi-20261020.log，64.5秒）。不调用外部pip/uv，不写项目声明或锁，不复制项目runtime。

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
| `packages/src/python_environment/prefix/verify.rs`、`src/core/libraries/package/tree.rs` | F3最终证据交接：原verify核返回同次按C4 receipt及已知wrapper/facts字节核验的TreeInventory；保留原receipt至发布前复验，不能将C4后同inode篡改重新观测为合法Ready。 |
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
| `src/core/libraries/package/tree.rs`内测试 | `f3_changed_same_inode_after_wheel_copy_is_never_published_ready`：真实C4完成后原inode等长改正文，原receipt核验必须拒绝且无Ready，不用新观测覆盖制品证据。 |
| 新增`packages/src/python_environment/shared_runtime/tests.rs`，扩F2的`src/core/programs/install/apply/tests.rs` | `f3_shared_runtime_has_no_full_copy`、`f3_failed_apply_keeps_old_selection`：真实语言租约与标准库来源、关联失败保留旧选择、项目声明未改。 |
| 新增`tests/e2e/packages/test_native_install_shared_wheel`、`test_native_deps_install_preserves_declarations`、`test_native_deps_no_manager` | 真实CLI安装固定完整版本、带传递依赖和console script的公开wheel；真实import/版本/路径/入口，A/B复用且选择隔离，不调用pip/uv或自动prune。 |

**阶段边界/退役条件：** 不创建`package/builds.rs`或源码构建协调器；sdist/URL/Git/local/editable/Conda、deps add声明编辑、升级、完整可移植锁/冷恢复、多文档事务和完整平台矩阵后移。现有完整pin必须遵守，不支持的锁模式拒绝，不能降级重求解。只有这条真实新链接通并由用户验收后，才删除其重复旧正文；`runtime_snapshot.rs`和旧store尚有消费者时不整删。

### F4：查看库内容及关联，删除明确指定的语言和包

最终只读接线补充：`src/core/programs/library.rs` 复用同次 `ObservedProjectInputs` 报告项目根、有效包声明、当前Python与是否存在旧生态声明；`cli/src/commands/{ls,current,project}.rs` 的共享Python分支移除多余旧 `Config::get()`，避免Tracker/monorepo marker写入。非Python原视图仍用原入口；不重写旧配置parser。独立 `tests/e2e/cli/test_f4_python_queries_are_readonly` 验证完整但缺项的Python查询不会初始化三库、写追踪或项目元数据，加入F4清单与原runner。

**状态：** ☑ 测试先行/实际RED；☑ 真实只读查询、双向引用与指定删除；☑ 本轮授权的Agent实测验收；☑ Spec/Standards审查。12项pakro核心、pakro-store完整54项、查询/删除CLI及纯Python只读CLI均通过。包括实际包--instance删除、持有租约与项目/依赖引用拒绝、部分删除撤销Ready、损坏内容与丢失已提交catalog不得当空、只读命令不触发旧Tracker。证据f-core-final、f-store-final、f4-cli-accepted、f4-query-cli-green-20261020.log。

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
| `packages/crates/pakro-store/src/control/write.rs`、`packages/crates/pakro-store/src/control/tests.rs`；既有 `src/core/libraries/project/repository.rs` | F4 缺失权威保护的必要接缝：仅由原稳定锁 `create_new` 获胜后的初始化回调产生本次 writer 新初始化事实，普通重开或竞争失败不能取得该资格。ProjectRepository 只由该初始赢者、在创建项目锁前经原 catalog CAS 发布真实空 catalog；已初始化库后续缺 catalog 一律未知并拒查/拒删。复用并发初始化夹具验证资格唯一，不改 Anchor/marker 格式、不新增第二权威或自动 repair。 |

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

### 第一版之后再按使用反馈推进

以下是后移，不是永久删除；恢复相应功能时，同时补它真正需要的保护和测试。

| 后移内容 | 原任务 | 为什么不挡首版 |
| --- | --- | --- |
| 自动编辑声明、完整可移植锁/恢复、通用多文件事务及崩溃恢复矩阵 | R09/R14/R15、R17/R22–R24的扩展部分 | 最低精确版本/项目依赖关联和实际写入保护已纳入F1–F4，后移的是其余完整变更能力。 |
| 批量prune、复杂回收、强制变更及完整中断恢复矩阵 | R21/R25及完整R07的其余部分 | F4已包含受引用保护的指定删除；首版不自动GC、不连带清其他对象。 |
| 其余查询输出、远端比较、诊断与所有辅助命令接线 | R18/R19/R26/R43的其余部分 | 库内项目/语言/包及关联查询已纳入F4，其他扩展按使用补接，不合并原职责。 |
| URL/Git/本地源码、完整源码构建、editable、Conda、语言源码安装 | R27–R34的剩余范围 | F首版仅承诺指定来源的兼容wheel和预编译Python；后续S阶段已补registry纯Python `.tar.gz` 的PEP517/legacy构建，解除该有限部分后移。编译器源码后端及其他来源/生命周期仍未完成，不偷偷调用外部管理器。 |
| 多私有源/认证组合与完整跨平台矩阵 | R10及各生态矩阵的扩展部分 | 单一已支持来源仍保留凭证与来源检查，未覆盖组合不伪称支持。 |
| 全仓旧链退役、工程入口拆分、生成资源与依赖大清理 | R39–R44 | 不为清理而先重构半个仓库；当前消费者确已替换时才做局部退役。旧数据清理另需用户授权范围。 |
| Java、Node、IDE/外部进程全生命周期 | 原M-PY/M-JAVA、R38/R35、R20 | 先完成可用Python，再按既定生态顺序扩展；首版不是整个Python阶段或产品最终验收。 |

### 缩减测试与防护工作的方法

- 已实现的摘要/来源校验、受控解包、锁/租约和发布内核直接复用，不拆掉后再等事故重写。
- 当前确实会执行的写入不能覆盖用户文件、修改其他项目共享内容，半成品不能报成功；不支持的修改/删除入口直接拒绝，不先实现全套恢复能力。
- 每个阶段交一条真实使用链及关键失败例；只验证本次改动和受影响的旧调用，不重复跑未变化的全套，不提前扩展跨平台/极端故障矩阵。
- 使用中发现问题：先定位，补最小复现，再修对应路径。不要顺手把未来所有边界都加成当前阶段的前置。
- 本轮编译、测试与格式检查由Agent按600秒上限自行执行并保存真实日志；0 tests、全ignored或仅编译成功不算功能交付。
- 保持同一构建配置，继续采用已有关闭增量缓存/调试信息的手动方案，不无故生成多套产物；不自动清理用户数据或提交Git。

### 各阶段的验收与复跑入口

同一 `tests/scripts/f-manual-check.sh` 已支持F1–F4、`--list`、`--format-only`与第二参数`--lib-only`，四份用例清单及8条CLI脚本均已注册，实际`--list`通过。F4另运行pakro-store初始化资格用例，不混算pakro测试数。复用原cargo/`tests/e2e/run_test`，没有新测试框架。Agent执行时另加600秒进程组上限及独立ASCII TMPDIR；用户仍可用下表入口复跑，不要求用户代替本轮测试。

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

复跑时使用带`cd '/Users/fuxinao/文件/project/pakro'`、真实输入前提的完整可复制命令，并依据当时已实现接口核实清单；当前入口已实际运行和核对。

### 已完成基础与证据

| 已完成 | 可复用成果 |
| --- | --- |
| R00–R03 | 基线、程序请求/上下文、真实配置观测与项目快照。 |
| R04–R05 | 内容身份与项目选择分离、受控IO、稳定锁和统一内核。 |
| R06 | 唯一共享Python、实际租约、发布/复用、来源证明与真实共享验收。 |
| F1–F4 | 本机首版目录选择、显式精确Python安装、真实共享wheel/项目关联、只读查询与受引用保护的指定删除；64项不同核心、8条CLI及实际公开PyPI验收通过。 |

R06合并150项不同测试通过；三包非测试编译、37文件格式与diff检查通过，详见[交接记录](HANDOFF.md)和[完成日志](log/2026-10-18-4-R06共享语言库.md)。原详案的完成状态与文件/测试合同原样存档，不把后移任务写成已完成。

**首版F1–F4已完成本轮验收。** 当前实现与证据见HANDOFF及2026-10-20日志。首版以外的后移能力仍需用户另行安排，不自动启动历史R07或Java/Node。
