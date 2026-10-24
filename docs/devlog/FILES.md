# 代码复用、改造与删除清单

**当前说明（2026-10-25）：用户已确认P1本机macOS arm64产品验收通过，自动多进程运行器禁用，并授权提交既有P1实现、测试及收尾文档。本轮仅改文档，不改代码。** 下表记录实际文件与消费者，验收证据见TESTS；长测试继续由用户终端执行。P2尚未启动，整个项目停用dev-flow skill。

文件范围依据[PLAN的P1.2逐文件表](PLAN.md)，并与本次`git diff --name-only`及未跟踪文件清单核对；额外接缝均列明实际调用和原因。工作树原有的主架构、README及开发文档修改予以保留，不把全部未提交文档归为本次代码改动。下方2026-10-18历史正文保持原样，不将当时状态重新解释为当前授权。F/S基线见[交叉盘点](ARCHITECTURE-REVIEW-2026-10-24.md)，验收证据由主任务写入[TESTS](TESTS.md)及[本轮日志](log/2026-10-24-6-P1实施.md)。

项目级入口 `AGENTS.md` 仅把“停用dev-flow”和权威文档位置提前提供给后续Agent，不新增第二套架构或授权。

## 2026-10-25定向修复接缝

- `install.rs`：在root目标早return前读取/缓存完整扩展计划；`config/mod.rs`、`task/mod.rs`、`task_tool_installer.rs`仅在只读规划时严格保真未知/remote错误并避免无关模板求值。
- `which.rs`：默认共享候选优先，同名与env/hook一致，保留已装NonPy companion和显式定向。
- `p1-watchdog.py/test-p1-watchdog.py`：撤去无法证明完整性的树快照执行；默认拒绝，显式受限单进程内核执行单列。`p1-manual-recheck.sh`为用户终端长验证入口，不能暗中绕过为Agent运行。
- 新E2E `test_p1_expanded_python_admission`、`test_p1_shared_command_priority`已由用户构建和实测通过（原始all-002758日志）；自动运行器禁用不追加为产品验收门槛。

## 本轮审查修复接缝

- `install.rs/use.rs`：准入覆盖实际后续配置包组和写目标；非Python目标不再先安装/保存后才进入旧Python链。
- `backend/asdf.rs/external_plugin_cache.rs`、`plugins/script_manager.rs`、`toolset_env.rs/toolset_paths.rs`：直接source与缓存、readonly错误保真和静态bin路径共用保护。
- `env.rs/which.rs/bin_paths.rs/mod.rs`、`toolset/mod.rs`、`query/snapshot.rs`：有效追加scope、实际companion路径与alias options一致判断。
- 新增4条review/冷安装E2E及固定PBS HTTP fixture；新增`p1-watchdog.py/test-p1-watchdog.py`与资源前后比较driver/测试fixture。
- A14原版采用隔离git archive及仅cfg(test)计数/注册改动；不改原生产实现，不替换当前工作树。

## 当前P1实现文件与消费者

| 实际文件 | 消费者及本次接线 | 保留边界或新增原因 |
| --- | --- | --- |
| `src/core/programs/query/environment.rs`（新）、`query/snapshot.rs`、`query/mod.rs`；`src/core/programs/directory.rs` | 共同查询核接语言、环境、包图及命令入口；目录适配消费同一观察结果，将只读描述与已有内容的关联应用分开。 | 区分无声明、缺语言、缺包图、歧义、损坏等结果；显式目标不借用cwd另一版本的包图，使用原取消/期限和完整对象核验。 |
| `src/core/config/mod.rs`；`src/core/libraries/project/runtime_pin.rs` | 冻结发现接真实profile/global/system/override/no-config等输入；运行时pin读取当前Python逻辑别名对应的锁键。 | 复用既有配置发现、held观察及锁解析；不另建配置或锁权威，不写统一项目锁。 |
| `src/core/programs/library.rs`；`cli/src/commands/{where,which,bin_paths,env,current}.rs` | 本地路径、解释器/console入口、环境及当前版本消费共同查询结果；明确目标/provider先分流。 | 保留各命令文本/JSON职责；Python缺失不回旧安装目录，非Python独立请求保留相应分支；不支持的Python运行时选项明确拒绝。 |
| `cli/src/commands/ls.rs`、`installs/{mod,ls}.rs`、`project.rs` | 库库存和项目状态按查询意图接共享事实，区分空库、当前声明、已提交关联和可用状态。 | 不把空结果当成退出共享Python路由的依据；project仅调整查询投影，未新增登记/声明事务。 |
| `cli/src/commands/{hook_env,hook_not_found,shim}.rs`；`runtimes/src/shims/mod.rs` | hook在关联应用后输出新环境；wrapper/shim/not-found复用明确provider归属与共享Python查询。 | Python缺项保持显式安装诊断；目录切换撤回Pakro环境差异；明确非Python执行在归属判定后恢复既有安装/执行能力。路径报告不授予外部进程运行租约。 |
| `src/core/programs/python.rs`、`install/{apply,mod}.rs`；`cli/src/commands/install.rs`、`deps/install.rs` | 显式Python安装、完整项目安装和deps install收敛到现有生产门面；原仅供测试的PBS-only apply重复正文退出生产注册。 | 继续复用真实F/S的runtime pin、完整根、wheel/source构建与提交核；list/explain和安装分开；精确bytecode恢复早分流保留。 |
| `src/core/programs/shared/python_entry.rs`及`python_entry/tests.rs`（新）、`shared/mod.rs`；`src/core/programs/python.rs` | 纯分类器消费解析后的backend/provider与操作种类，经有限门面供CLI和运行时调用；Python门面另承接旧库存的prune准入读取。 | 分类器本身不读取库、不扫描任意shell文本；库存不可读保留错误，不冒充空集合。 |
| `cli/src/commands/{use,upgrade,unuse,prune,install_into}.rs`、`deps/{add,remove,mod}.rs` | 未迁移Python声明变更、批量回收和Python provider在目标复核后早拒绝；无版本请求同样检查实际依赖；install-into在handler开头全部拒绝。 | 混合批次不能先改非Python部分；独立非Pythondeps操作排除cwd中无关Python补装。D01仅落实执行拒绝，完整命令树/Schema退役未在P1展开。 |
| `cli/src/commands/mod.rs`、`command_effects.rs`；`runtimes/src/toolset/install_state.rs` | `run_inner`在解析阶段只加载本地元数据，维护前调用命令准入；本地观察分支绕开自动更新、registry刷新、迁移、cache和purgatory维护；效果说明反映隐式安装与hook关联写入。 | 保留别名、`--cd`、根flag及更早的精确恢复分支；只读安装状态扫描不采纳旧安装、不写迁移manifest、不删除插件。 |
| `runtimes/src/toolset/toolset_install.rs`、`runtimes/src/backend/mod.rs`、`runtimes/src/toolinstall/install_context.rs` | 旧批次在plugin准备前检查目标及依赖元数据；Backend直接install/force在prepare前拒绝，postinstall在环境求值/脚本前拒绝；复用原metadata validate传播未知目标错误。 | 直接调用仍保留底层共享路径保护。只读且空批次直接返回，使已就绪共享环境可经过Exec检查而不顺带安装env-only插件；普通非Python空批次语义保留。 |
| `src/core/config/mod.rs`、`config/config_file/mod.rs` | Config加载中的Tracker、monorepo marker和trust持久化受只读调用标志约束。 | 这些写入发生在handler之前，单有handler准入不足；已信任输入继续读取，不能在观察阶段提示后持久化信任或忽略决定。 |
| `src/core/config/remote_include.rs`、`config/env_directive/mod.rs`、`src/core/util/tera.rs`；`runtimes/src/backend/mod.rs` | Config实际加载链的remote include、vars/source/module/建venv、模板exec和plugin idiomatic元数据执行分别受只读能力约束。 | 新接缝来自真实可达的下载/执行/创建路径；保留纯模板求值与内置只读元数据，对无法静态确定的动态输入明确诊断，不静默运行后再拒绝。 |
| `runtimes/src/shims/fake_asdf.rs`、`plugins/script_manager.rs`、`plugins/asdf_plugin.rs`、`backend/asdf.rs` | plugin对象构造只计算PATH；`.fake-asdf` helper创建移到真正脚本执行，ScriptManager受准入保护并向调用者传播错误。 | 实际CLI快照暴露`backend::load_tools → ScriptManager::new → INITIAL_ENV → setup`提前创建helper，故增加这些接缝；正常非Python插件实际执行仍可按需创建。 |
| `runtimes/src/toolset/env_cache.rs`、`src/core/util/hint.rs` | 只读调用禁用两类环境cache；真实TTY提示继续显示，但不写“已显示”标记。 | Ready环境/Exec会进入env cache保存路径，TTY current会进入hint持久化路径；这两者不能从只读启动分支重新引入状态写入。 |
| `src/core/programs/directory/tests.rs`、`query/paths/tests.rs`、`install/apply/tests.rs`、`python/integration_tests.rs`；上述CLI/Backend/Toolset文件内测试 | 补显式目标、冻结观察、前缀歧义、别名pin及保留F/S断言；准入测试包含真实Backend force/postinstall和真实Toolset批入口。 | 测试文件存在仅表示用例已落盘；不以纯分类器断言替代生产入口验证，不在本表记录通过状态。 |
| `src/core/programs/directory/resource_baseline.rs`（新）；`src/core/libraries/{language,package}/lease.rs` | 有限可见的既有测试计数供固定大图fixture记录完整核验次数、选择/关联与FD变化。 | lease改动仅限`cfg(test)`计数可见性；不改生产持有模型、hash/namespace检查、300秒预算或4096软FD目标。 |
| `tests/e2e/cli/test_p1_{shared_python_queries,shared_python_empty_queries,python_write_admission,readonly_initialization,non_python_command_dispatch,query_option_admission}`（新）；`tests/e2e/run_test` | 真实CLI覆盖共享/空/损坏库、目标及scope、写准入、启用维护设置的状态快照/HTTP计数、TTY提示、独立非Python派发和查询选项；原runner透传显式归档输入。 | 复用既有隔离HOME、HTTP夹具及真实runtime/wheel入口；不操作用户库/ISAT/真实4-pyc，不把不存在的Ready目录伪造为可用语言。 |
| `tests/scripts/p1-check.sh`、`p1-test-cases.txt`、`p1-resource-baseline.py`（新） | 按实际注册核对非零用例、单Cargo及统一target/profile运行有界组，记录资源观测和完整日志。 | 每组共同600秒上限；失败/超时和清理结果分别保留，不以拆组绕过某一超时场景，不硬编码本机测试binary哈希。 |

### 本会话接线记录（2026-10-24）

本次文档补充仅更新本文件顶部，保留其后历史清单。授权从“方案细化”进入P1实现，故撤下原“代码未开始”的当前说明；没有把P2、通用联合事务、声明写入、完整来源接入或批量回收纳入本次实现。

实现先统一观察和消费，再对旧写入口及启动前置链加入准入。新增配置、plugin、helper、hint和env-cache接缝均对应上表列出的实际调用路径；其中`.fake-asdf`提前创建来自真实CLI状态快照定位。问题定位与修复代码分别记录，不在本文件把“已修”写成“验收通过”。无版本依赖检查复用Toolset未求解请求和原依赖metadata核；未知metadata传播错误，force/postinstall另保留真实直接入口断言。

本表基于本次读取时的未提交工作树，包含已修改及已新增文件；最终文件范围和测试结果仍由主任务集成确认。此次只改`docs/devlog/FILES.md`，未运行Cargo、未启动P2、未调用dev-flow、未提交Git。

## 历史清单及当时进展（2026-10-18）

日期：2026-10-18。**本清单记录目标处置及当前进展；R00 的 reshim 条件编译修复已获用户 release 构建通过；R05 已提取受控IO、锁和有界帧，原业务owner保留。** 以当前工作树（含未提交/未跟踪文件）为准，不恢复用户已清空的旧 docs。

**本轮范围已确定：Python → Java → Node，先完善Python。D01已取消install-into：其代码列为明确退出项，下载和安装统一由Pakro库管理。R00–R06 已完成；用户要求先不启动R07，不提前执行后续退出或删除。**

2026-10-18新增D07使用方式修订：进入项目目录按配置选择库内已有语言/包，缺项由用户决定安装；允许必要内部适配但不增加创建/激活环境步骤。仅改文档，业务代码未接线；必要目录hook不在整文件退出范围，见下方D-CLI标注。

2026-10-18新增D09/D10：当前PLAN为F1–F4，首版包括项目/语言/包及关联查询、无引用才允许的指定删除；F1–F3须维护最低精确关联，下载请求和项目直接清单须完整精确版本。下表R编号保留资产定位用途，不恢复原详案全部前置；本次未处置任何代码文件，未启动实现。批量prune与force等仍后移，具体边界以主架构和当前PLAN为准。

同日逐文件细化：当前[PLAN](PLAN.md)已分别列F1–F4的修改、新增、只复用文件和局部退役条件，是首版的直接文件范围。F1接本地选择与同catalog登记，F2接显式安装和旧路径拒绝，F3接真实wheel/共享runtime，F4接只读关联和指定删除；下方旧R映射不意味着必须提前完成对应模块全部内容。任何新文件目前都只是计划，本次没有修改或删除源码。

R00 当前改动：`cli/src/commands/mod.rs` 移除 `mod reshim;` 前的 `#[cfg(debug_assertions)]`，使模块声明与现有 `Commands::Reshim`、`asdf` 转发一致。用户已提供 E0432/E0433 失败证据及 release 构建完成日志；只证明本次构建修复通过，`reshim.rs` 的 R43 退出候选状态不变。

R00 运行器：复用 `tests/scripts/test-e2e-runner.py`，先获 14 项/6 处失败；修复 `tests/e2e/run_all_tests` 的空数组与报告后，同一组 14 项已由用户全部通过（8.265s）。`run_test`、并发、重试、实际安装入口及全部测试断言未改。不新增框架，不将此步说成产品 E2E 已通过；R00 处置完成。

R01 已完成：`src/lib.rs` 注册 public programs/private libraries 并保留开工前已有改动；`src/core/programs/` 的请求、冻结上下文、报告和有限公开 facade 接通。normalize_intent 直接消费原 parse_install_arg/requests_from_bound；`packages/src/request.rs` 提取的 select_bare_package_runtime 由旧 split 与新入口共用。OwnedPackageRequest/ToolSource/ConfigScope、Settings、CancellationToken、PythonRegistry/SourcePolicy 和目标描述复用，无 parser/来源实现副本，binding.rs 未改。

`src/core/libraries/mod.rs` 仅向 context 提供词法位置描述，返回库内错误，在程序层转换；不创建目录、不反向依赖 programs、不授予实际 IO。`shared/report.rs` 保留调用方描述引用而不发明 R04 身份。`tests/r01_program_contract.rs` 从独立 crate 消费必要公开接口，内部 owner 不公开。模块30项、外部6项、原解析8项及定向格式检查通过；没有退役原实现，真实 owner/租约/事务/CLI管线留后续任务。

R02 已完成：`native_python.rs` 直接从 ReadOnlyFileObservation 的 held bytes 调原 PakroToml::from_str，解析后复核；checked 请求前后及新 receipt 创建前再次复核，同 owner 指针校验保留。`pakro_toml.rs` 只新增 pub(super) checked 只读投影，直接复用 requests_from_config，未改 parser 或旧消费者。原六项测试保留，新增五项本机用例通过，另有 Linux 专用非 UTF-8 别名用例未在当前 macOS 执行。只删除 native 的 NotImplemented；原信任/日志策略不变，不把此 owner 当完整非交互查询管线。

R03 已完成：`project/snapshot.rs` 复用 ReadOnlyFileObservation、BoundPythonProjectFile 和原包声明合并核；新增 ReadOnlyPathObservation 在原 Namespace/DirectoryEdge/AliasPin 上观察目录及首个缺失节点，没有复制IO解析。发现逻辑共享原filename/profile/fragment/ignore纯核，以显式冻结输入替代全局缓存。原Config、Tracker、remote_include仍供旧CLI使用，尚未退役；新入口不调用它们。

`project/declarations.rs` 用同次held bytes及原decor helpers生成保真native文档，无实际写盘；同值保持整份字节，其他格式/不可静态改写的selector明确诊断。`install/context.rs`复用R01归一化、纯环境键/选项/别名解析，验证作用域与有效候选，借用同一观测。`lockfile/mod.rs`只提取原路径计算的纯参数接缝；没有构建新锁图、guard或事务。58项R03与65项原核回归通过；没有删除源码或用户数据。细节见本轮日志。

R04 已完成：`libraries/language/identity.rs` 与 `package/identity.rs` 复用Digest、NormalizedPythonName/Pep440及统一framing核，区分ArtifactId/OutputDigest/BuildRequestId和语言/包/组合键。`project/selections.rs`只委托现有CatalogModel，借其同一选择表与codec提供ProjectSelection和typed references，不另建权威。

`python_environment/model.rs`去掉GenerationRecord的单项目owner，以target与显式shared_layout区分内容描述；派生索引只加速共享键查找。原revision/CAS/tombstone/nonce及retired保护保留，共享登记在所有入口与decode/state校验中保持唯一。codec在原bounded seeds/streaming writer上升级schema2，schema1明确拒绝，不新建迁移管线。journal的target核验与新kind算法校验在原read/encode/prepare内接通；旧catalog/prefix测试同步显式target构造，原保护逻辑不删。当前45项R04与90项原逻辑回归通过；没有删除模块、旧数据或引入实际共享发布权限。

R05 已完成：`pakro-store/control`提供分离的ReadStore/StoreWriter、稳定inode锁、OrderedLocks及不解释业务语义的有界帧。原prefix/control根打开、目录安全检查、CAS/read/fsync和Flock正文已下沉，旧入口委托唯一内核；无消费者的open_owned_regular创建正文删除。原ownership/framing仅保留应用Header编码，read/append委托共用帧核；JournalRecord、selection、ownership events和CleanupWitness继续留应用层。

旧prefix保留实际Flock持有者并新增StableLock pin，使用原deadline持续检查build保护。recovery对象锁提前到catalog读取之前；scan显式进入metadata阶段；cleanup沿用同一个kernel。prefix及control旧夹具适配初始化时机，原安全断言保留。store的eyre/nix/uuid只接已有版本；未改旧store.rs、其他backend/util锁、生产CLI或用户数据。282项本机行为和非测试库检查已通过，后续R06/R15/R21仍各有独立门。

R06已完成：`libraries/language/{manifest,repository,lease,publication}.rs`复用R05 StoreWriter/ReadStore、稳定SH/EX及CAS，固定唯一payload位置后受控提取、probe、完整inventory及Ready发布。`programs/install/runtime.rs`负责lazy获取、blocking调度、同一取消/deadline和已有实例SH下按需来源重验。原native/extract任意Path与remove_all正文删除，测试迁入同owner核；旧兼容目录只引用共享payload，未触碰用户既有解释器。

`prefix/verify.rs`提取唯一完整树inventory核，旧prefix wrapper继续委托；`PythonRuntimeHandle`持有私有RuntimeProtection、克隆保留实际SH并在probe前后复核。native plan复用原身份、locator和真实probe/target标签核，sysconfig只提取原纯patch函数，bzip2只连接既有版本。来源证明使用同一个VerifiedArtifact的SHA256；util/Sigstore只显式传冻结路由/重试/TUF与摘要，调用原签名核，旧API保留。

R06最后接缝为`native/provenance.rs`与14项`provenance/tests.rs`：新安装consumer复用R05目录/权限/inode/no-clobber primitives及原proof字节/文件名核，读缺失零创建，写权限沿libraries，损坏/链接/替换拒绝且不覆盖。用户180052.log与182044.log合并确认150个不同用例全部通过，含真实共享两项、proof14项、Sigstore代理隔离修复与util回归，三包非测试check通过；最终182740.log确认37文件格式与diff检查通过。R06没有剩余验收项，不无故重跑。R07旧旁路、R13整体布局、R15提交和R16 CLI仍属后续范围，用户要求当前不启动R07。

## 状态定义

- **保留**：已符合架构且有功能/协议价值，继续由同一实现提供。
- **改接或提取**：修改输入、所有权或调用关系；提取后只保留一个内核。
- **替代后删除**：新consumer真实接线、用户行为验证通过、所有生产/构建/测试引用核清后才删。
- **范围退出**：用户已排除的产品功能，先解除核心依赖，再删除专属实现/资源。
- **待裁决**：主架构已有未决事项；不能在清理中间接替用户决定。

删除门：①替代任务明确；②源调用、动态分派、include/构建生成、Schema/派生资源都核对；③有价值测试断言先迁移；④用户实际测试结果支持；⑤只删批准的代码，不运行用户数据清理。任一条件缺失，条目保持未完成。

## K：优先保留的资产

| 区域 | 当前文件/模块 | 任务 | 处理说明 |
| --- | --- | --- | --- |
| 运行时与平台 | runtimes/src/plugins/core/python/native/；runtimes/src/toolinstall/python_runtime.rs；packages/src/python_target.rs | R06/R07/R13/R33 | 保留发行发现、校验、真实目标、组件与测试；改生命周期/共享接口，不重新实现探测。 |
| 包来源与缓存 | packages/crates/pakro-index/src/{artifact,python_source,python_tags,python_metadata,python_requirements,python_markers}.rs；simple/ | R08/R10/R11 | 复用显式会话、认证、原始制品验证与原生语义；旧Index::from_env不自动作为新入口。 |
| 纯解析器 | packages/crates/pakro-resolver/src/{python_native,pyver,graph,solve}.rs及子模块 | R11/R35 | 保留Python已经细化的语义；Node实例图改造不得破坏Python纯核。 |
| 包安装核 | packages/src/python_wheel*.rs及其子模块 | R12/R13/R31 | 保留ZIP/RECORD/metadata/scheme/entry points完整检查；仅改受管输入/共享链接。 |
| 声明与锁模型 | packages/src/python_declarations*；python_project*；python_resolution/{root_scopes,lock_model,lock_session}* | R02/R03/R11/R14 | 保留原始声明、作用域、保真文档和真实owner；未完成接口接通后才声称可用。 |
| 制品快照 | src/core/artifact_snapshot.rs及子模块 | R08/R10 | 这是已校验字节的持有与缓存，不是完整运行时复制。不得因名字有snapshot被删。 |
| 受控IO和租约 | packages/src/python_environment/prefix*；catalog/journal/model | R04/R05/R15/R21 | 提取真实FS/锁/owner/codec核；修改旧单项目owner和旧锁序；不要复制成三套。 |
| 锁文件IO | src/core/lockfile/transaction.rs及子模块；src/core/lockfile/mod.rs | R14/R15 | 保留受控CAS/fsync/保真与工具锁兼容，单文件IO不是联合事务完成。 |
| Node现有能力 | packages/crates/pakro-index/src/npm.rs；packages/src/materialize_node.rs | R35 | 保留registry/semver/integrity/bin；多实例/peer/共享引用按新接口改造。 |
| Conda原生能力 | runtimes/src/backend/conda.rs | R34 | 提取Rattler/Resolvo协议到唯一实现，不能整文件删掉原生资产。 |
| 本轮语言发行 | runtimes/src/plugins/core/{python,java,node}.rs | R33/R38/R35 | 按Python→Java→Node实施；其他语言已有合规资产暂留，不安排本轮生态开发，也不按范围缩小机械删除。 |
| 安全与基础库 | src/crates/pakro-util、pakro-cache-core、pakro-sigstore、pakro-settings、pakro-dotenv | R42/R44 | 按真实消费者裁剪，不因旧功能使用过就整库删除。 |
| CLI表现与测试 | 23 个本轮核心入口、现有旧 migrate 入口及 D04 保留辅助入口的参数/输出用例 | R16–R26、R43 | 保留独立职责；旧 migrate 不扩建、不擅删。CLI 测试是 --bin pakro。 |

## D-PY / D-DEPS / D-REFS：重复与不合规生产路径

| 当前锚点 | 退出内容 | 替代任务 | 删除前必须满足 |
| --- | --- | --- | --- |
| [packages/src/install.rs:501](/Users/fuxinao/文件/project/pakro/packages/src/install.rs:501) | build_sdist_to_wheel | R30/R31/R33 | 原生源码链已实际CLI接线并用户通过；先迁移旧测试的错误/校验断言。 |
| [packages/src/install.rs:75](/Users/fuxinao/文件/project/pakro/packages/src/install.rs:75) | prepare_python_artifacts旧链 | R10/R11/R12/R16 | held artifact + 完整wheel consumer真实替代。 |
| [packages/src/python.rs:224](/Users/fuxinao/文件/project/pakro/packages/src/python.rs:224) | 旧PythonSource provider | R11/R16/R26 | 旧安装与查询caller分别迁完；python.rs仍有target/查询helper，不能整删。 |
| [packages/src/materialize.rs:30](/Users/fuxinao/文件/project/pakro/packages/src/materialize.rs:30) | materialize_venv旧项目物化 | R12/R13/R16 | 共享布局及其消费者通过；先迁移状态/入口查找和有价值断言。 |
| [packages/src/python_environment/runtime_snapshot.rs:89](/Users/fuxinao/文件/project/pakro/packages/src/python_environment/runtime_snapshot.rs:89) | 生产完整runtime复制 | R06/R13 | 安装/构建生产消费者转 LanguageLease，R20 外部运行接入后置，不阻塞共享替代；测试仍用片段单列，不把新共享 owner 重命名成旧 copy。 |
| [packages/src/python_environment/runtime_bootstrap.rs:565](/Users/fuxinao/文件/project/pakro/packages/src/python_environment/runtime_bootstrap.rs:565) | 整套runtime输入复制 | R13 | 真实CPython/PyPy最终布局与标准库来源通过；保留实际startup验证。 |
| [packages/src/binding.rs:182](/Users/fuxinao/文件/project/pakro/packages/src/binding.rs:182) | Python sidecar migration guard | R11/R14/R16 | 真实冷Locked/Offline恢复可用，旧锁缺事实仍有明确诊断。 |
| [packages/src/lockfile.rs:17](/Users/fuxinao/文件/project/pakro/packages/src/lockfile.rs:17) | PackageLock sidecar写入 | R14/R35/R39 | Python/Node 等全部写消费者迁完；不新增旧格式迁移，R39 单列旧测试数据清理。 |
| [packages/crates/pakro-store/src/store.rs:172](/Users/fuxinao/文件/project/pakro/packages/crates/pakro-store/src/store.rs:172) | 逐项目路径refs与旧gc | R04/R05/R09/R21/R35 | 新引用权威/所有生态迁移后退出，不留第二套可写refs。 |
| [packages/src/install.rs:363](/Users/fuxinao/文件/project/pakro/packages/src/install.rs:363) | maybe_auto_gc 旧自动调用 | R21/R23 | 退出普通安装后的全库自动 GC 调用；用户显式 prune 才筛选无项目引用的正式对象。本次操作自己的暂存收尾仍受所有权与内部使用保护。 |
| [runtimes/src/plugins/core/python.rs:1060](/Users/fuxinao/文件/project/pakro/runtimes/src/plugins/core/python.rs:1060) | python-build源码委托 | R33 | 对应原生源码配方和组件矩阵通过。 |
| [runtimes/src/plugins/core/python.rs:1092](/Users/fuxinao/文件/project/pakro/runtimes/src/plugins/core/python.rs:1092) | 默认包 python -m pip | R07/R33 | D06 已取消自动附带常用包；退出默认清单读取与安装调用，不转为公共/项目声明；独立验证不再自动装包后删除，不等源码构建全链完成。 |
| [packages/src/system.rs:11](/Users/fuxinao/文件/project/pakro/packages/src/system.rs:11) | 旧Go/Maven manager桥 | R38/R40 | 本轮只实施Java/Maven替代；Go不开发新生态，其委托不能经本轮核心入口回退执行。保留可复用资产，不按system文件名整删。 |
| [packages/deps/engine.rs:1](/Users/fuxinao/文件/project/pakro/packages/deps/engine.rs:1) | 核心manager执行编排 | R23/R24/R38/R35 | 原生命令接线替代后删命令执行，声明识别按需要保留。 |
| [src/frontend.rs:1](/Users/fuxinao/文件/project/pakro/src/frontend.rs:1) | core反向调用CLI提交 | R15/R16/R17/R22/R40 | 真实提交逻辑迁入program；输出/可用基础原语仍有消费者时逐项处理。 |

### deps provider 逐文件处置

以下文件中的“返回/执行外部管理器命令”属于退出部分；不能把有用的静态manifest识别、版本/作用域解析也无差别删除。对应生态未完成时，新入口明确报能力缺口，不恢复fallback；完整重构完成前仍须关闭选定范围的能力缺口。

- [packages/deps/providers/bun.rs](/Users/fuxinao/文件/project/pakro/packages/deps/providers/bun.rs)
- [packages/deps/providers/bundler.rs](/Users/fuxinao/文件/project/pakro/packages/deps/providers/bundler.rs)
- [packages/deps/providers/composer.rs](/Users/fuxinao/文件/project/pakro/packages/deps/providers/composer.rs)
- [packages/deps/providers/custom.rs](/Users/fuxinao/文件/project/pakro/packages/deps/providers/custom.rs)
- [packages/deps/providers/dart.rs](/Users/fuxinao/文件/project/pakro/packages/deps/providers/dart.rs)
- [packages/deps/providers/deno.rs](/Users/fuxinao/文件/project/pakro/packages/deps/providers/deno.rs)
- [packages/deps/providers/git_submodule.rs](/Users/fuxinao/文件/project/pakro/packages/deps/providers/git_submodule.rs)
- [packages/deps/providers/go.rs](/Users/fuxinao/文件/project/pakro/packages/deps/providers/go.rs)
- [packages/deps/providers/mod.rs](/Users/fuxinao/文件/project/pakro/packages/deps/providers/mod.rs)
- [packages/deps/providers/npm.rs](/Users/fuxinao/文件/project/pakro/packages/deps/providers/npm.rs)
- [packages/deps/providers/pip.rs](/Users/fuxinao/文件/project/pakro/packages/deps/providers/pip.rs)
- [packages/deps/providers/pnpm.rs](/Users/fuxinao/文件/project/pakro/packages/deps/providers/pnpm.rs)
- [packages/deps/providers/poetry.rs](/Users/fuxinao/文件/project/pakro/packages/deps/providers/poetry.rs)
- [packages/deps/providers/uv.rs](/Users/fuxinao/文件/project/pakro/packages/deps/providers/uv.rs)
- [packages/deps/providers/yarn.rs](/Users/fuxinao/文件/project/pakro/packages/deps/providers/yarn.rs)

## D-CLI：明确退出的公开入口专属源码候选

旧环境自动管理另外列入R40：`runtimes/environment/uv.rs:19`、`runtimes/environment/venv.rs:77` 及 `runtimes/src/toolset/mod.rs` 的调用者。共享布局和查询接通后退出uv/stdlib-venv自动创建；有价值的纯路径/环境事实投影继续保留，不能漏查这些隐式入口。

实施任务：R41（工程入口先解耦）→ R42（核心依赖先拆出）→ R43（取消路由并删除专属实现）→ R44（派生资源/依赖）。

下面保留原枚举清单，不是立即删除命令。共享helper仍有核心调用时转为“提取/保留”，不得为完成清单而删核心能力。原列表146个文件，加D01的install_into.rs共147个条目；D07已将activate/deactivate/hook_env中必要接入改为待拆分保留，147不再表示整文件删除数量。目录选择不能随通用Shell/task/daemon整组退出，也不因此恢复无关旧功能。

- [ ] [cli/src/commands/activate.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/activate.rs) — D07：目录感知所需接入待拆分保留，撤回整文件退出假设；不增加逐项目激活步骤。
- [ ] [cli/src/commands/asdf.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/asdf.rs)
- [ ] [cli/src/commands/backends/ls.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/backends/ls.rs)
- [ ] [cli/src/commands/backends/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/backends/mod.rs)
- [ ] [cli/src/commands/backends/switch.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/backends/switch.rs)
- [ ] [cli/src/commands/bootstrap.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/bootstrap.rs)
- [ ] [cli/src/commands/completion.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/completion.rs)
- [ ] [cli/src/commands/config/get.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/config/get.rs)
- [ ] [cli/src/commands/config/keys.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/config/keys.rs)
- [ ] [cli/src/commands/config/ls.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/config/ls.rs)
- [ ] [cli/src/commands/config/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/config/mod.rs)
- [ ] [cli/src/commands/config/set.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/config/set.rs)
- [ ] [cli/src/commands/daemons.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/daemons.rs)
- [ ] [cli/src/commands/deactivate.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/deactivate.rs) — D07：与最小接入配套的环境还原待拆分保留，不恢复独立环境管理流程。
- [ ] [cli/src/commands/direnv/activate.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/direnv/activate.rs)
- [ ] [cli/src/commands/direnv/envrc.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/direnv/envrc.rs)
- [ ] [cli/src/commands/direnv/exec.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/direnv/exec.rs)
- [ ] [cli/src/commands/direnv/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/direnv/mod.rs)
- [ ] [cli/src/commands/dotfiles/add.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/add.rs)
- [ ] [cli/src/commands/dotfiles/apply.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/apply.rs)
- [ ] [cli/src/commands/dotfiles/capture.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/capture.rs)
- [ ] [cli/src/commands/dotfiles/capture_health.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/capture_health.rs)
- [ ] [cli/src/commands/dotfiles/conflicts.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/conflicts.rs)
- [ ] [cli/src/commands/dotfiles/diff.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/diff.rs)
- [ ] [cli/src/commands/dotfiles/edit.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/edit.rs)
- [ ] [cli/src/commands/dotfiles/exclude.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/exclude.rs)
- [ ] [cli/src/commands/dotfiles/history/describe.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/history/describe.rs)
- [ ] [cli/src/commands/dotfiles/history/diff.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/history/diff.rs)
- [ ] [cli/src/commands/dotfiles/history/ls.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/history/ls.rs)
- [ ] [cli/src/commands/dotfiles/history/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/history/mod.rs)
- [ ] [cli/src/commands/dotfiles/history/show.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/history/show.rs)
- [ ] [cli/src/commands/dotfiles/history_status.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/history_status.rs)
- [ ] [cli/src/commands/dotfiles/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/mod.rs)
- [ ] [cli/src/commands/dotfiles/origin.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/origin.rs)
- [ ] [cli/src/commands/dotfiles/paths.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/paths.rs)
- [ ] [cli/src/commands/dotfiles/pull.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/pull.rs)
- [ ] [cli/src/commands/dotfiles/recover.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/recover.rs)
- [ ] [cli/src/commands/dotfiles/rollback.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/rollback.rs)
- [ ] [cli/src/commands/dotfiles/save.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/save.rs)
- [ ] [cli/src/commands/dotfiles/status.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/status.rs)
- [ ] [cli/src/commands/dotfiles/sync.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/sync.rs)
- [ ] [cli/src/commands/dotfiles/track.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/track.rs)
- [ ] [cli/src/commands/dotfiles/unapply.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/unapply.rs)
- [ ] [cli/src/commands/dotfiles/undo.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/undo.rs)
- [ ] [cli/src/commands/dotfiles/untrack.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/untrack.rs)
- [ ] [cli/src/commands/dotfiles/watch.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/dotfiles/watch.rs)
- [ ] [cli/src/commands/en.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/en.rs)
- [ ] [cli/src/commands/exec.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/exec.rs)
- [ ] [cli/src/commands/external.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/external.rs)
- [ ] [cli/src/commands/fmt.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/fmt.rs)
- [ ] [cli/src/commands/generate/config.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/generate/config.rs)
- [ ] [cli/src/commands/generate/devcontainer.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/generate/devcontainer.rs)
- [ ] [cli/src/commands/generate/git_pre_commit.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/generate/git_pre_commit.rs)
- [ ] [cli/src/commands/generate/github_action.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/generate/github_action.rs)
- [ ] [cli/src/commands/generate/install_script.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/generate/install_script.rs)
- [ ] [cli/src/commands/generate/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/generate/mod.rs)
- [ ] [cli/src/commands/generate/task_docs.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/generate/task_docs.rs)
- [ ] [cli/src/commands/generate/task_stubs.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/generate/task_stubs.rs)
- [ ] [cli/src/commands/generate/tool_stub.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/generate/tool_stub.rs)
- [ ] [cli/src/commands/github/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/github/mod.rs)
- [ ] [cli/src/commands/github/token.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/github/token.rs)
- [ ] [cli/src/commands/global.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/global.rs)
- [ ] [cli/src/commands/hook_env.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/hook_env.rs) — D07：保留/改接目录变化、配置选择及环境差异应用；剥离task/daemon等非核心调用，不整文件删除。
- [ ] [cli/src/commands/hook_not_found.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/hook_not_found.rs)
- [ ] [cli/src/commands/install_into.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/install_into.rs) — D01已明确取消；R43同步删除注册、分派、帮助/生成资源及专属测试，不删除用户既有目录。
- [ ] [cli/src/commands/link.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/link.rs)
- [ ] [cli/src/commands/local.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/local.rs)
- [ ] [cli/src/commands/lock.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/lock.rs)
- [ ] [cli/src/commands/mcp.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/mcp.rs)
- [ ] [cli/src/commands/oci/build.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/oci/build.rs)
- [ ] [cli/src/commands/oci/common.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/oci/common.rs)
- [ ] [cli/src/commands/oci/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/oci/mod.rs)
- [ ] [cli/src/commands/oci/push.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/oci/push.rs)
- [ ] [cli/src/commands/oci/run.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/oci/run.rs)
- [ ] [cli/src/commands/packslip/forget.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/packslip/forget.rs)
- [ ] [cli/src/commands/packslip/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/packslip/mod.rs)
- [ ] [cli/src/commands/packslip/pins.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/packslip/pins.rs)
- [ ] [cli/src/commands/patrons.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/patrons.rs)
- [ ] [cli/src/commands/plugins/install.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/plugins/install.rs)
- [ ] [cli/src/commands/plugins/link.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/plugins/link.rs)
- [ ] [cli/src/commands/plugins/ls.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/plugins/ls.rs)
- [ ] [cli/src/commands/plugins/ls_remote.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/plugins/ls_remote.rs)
- [ ] [cli/src/commands/plugins/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/plugins/mod.rs)
- [ ] [cli/src/commands/plugins/uninstall.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/plugins/uninstall.rs)
- [ ] [cli/src/commands/plugins/update.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/plugins/update.rs)
- [ ] [cli/src/commands/publish_system_install.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/publish_system_install.rs)
- [ ] [cli/src/commands/reshim.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/reshim.rs)
- [ ] [cli/src/commands/run.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/run.rs)
- [ ] [cli/src/commands/set.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/set.rs)
- [ ] [cli/src/commands/settings/add.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/settings/add.rs)
- [ ] [cli/src/commands/settings/get.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/settings/get.rs)
- [ ] [cli/src/commands/settings/ls.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/settings/ls.rs)
- [ ] [cli/src/commands/settings/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/settings/mod.rs)
- [ ] [cli/src/commands/settings/set.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/settings/set.rs)
- [ ] [cli/src/commands/settings/unset.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/settings/unset.rs)
- [ ] [cli/src/commands/shell.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/shell.rs)
- [ ] [cli/src/commands/shell_alias/get.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/shell_alias/get.rs)
- [ ] [cli/src/commands/shell_alias/ls.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/shell_alias/ls.rs)
- [ ] [cli/src/commands/shell_alias/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/shell_alias/mod.rs)
- [ ] [cli/src/commands/shell_alias/set.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/shell_alias/set.rs)
- [ ] [cli/src/commands/shell_alias/unset.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/shell_alias/unset.rs)
- [ ] [cli/src/commands/skills/ls.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/skills/ls.rs)
- [ ] [cli/src/commands/skills/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/skills/mod.rs)
- [ ] [cli/src/commands/skills/sync.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/skills/sync.rs)
- [ ] [cli/src/commands/sponsors.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/sponsors.rs)
- [ ] [cli/src/commands/ssh.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/ssh.rs)
- [ ] [cli/src/commands/sync/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/sync/mod.rs)
- [ ] [cli/src/commands/sync/node.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/sync/node.rs)
- [ ] [cli/src/commands/sync/python.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/sync/python.rs)
- [ ] [cli/src/commands/sync/reconcile.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/sync/reconcile.rs)
- [ ] [cli/src/commands/sync/ruby.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/sync/ruby.rs)
- [ ] [cli/src/commands/system/brew/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/system/brew/mod.rs)
- [ ] [cli/src/commands/system/brew/tap.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/system/brew/tap.rs)
- [ ] [cli/src/commands/system/brew/untap.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/system/brew/untap.rs)
- [ ] [cli/src/commands/system/export.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/system/export.rs)
- [ ] [cli/src/commands/system/import.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/system/import.rs)
- [ ] [cli/src/commands/system/install.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/system/install.rs)
- [ ] [cli/src/commands/system/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/system/mod.rs)
- [ ] [cli/src/commands/system/prune.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/system/prune.rs)
- [ ] [cli/src/commands/system/status.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/system/status.rs)
- [ ] [cli/src/commands/system/upgrade.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/system/upgrade.rs)
- [ ] [cli/src/commands/system/use.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/system/use.rs)
- [ ] [cli/src/commands/system/where.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/system/where.rs)
- [ ] [cli/src/commands/tasks/add.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/tasks/add.rs)
- [ ] [cli/src/commands/tasks/deps.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/tasks/deps.rs)
- [ ] [cli/src/commands/tasks/edit.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/tasks/edit.rs)
- [ ] [cli/src/commands/tasks/graph.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/tasks/graph.rs)
- [ ] [cli/src/commands/tasks/info.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/tasks/info.rs)
- [ ] [cli/src/commands/tasks/ls.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/tasks/ls.rs)
- [ ] [cli/src/commands/tasks/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/tasks/mod.rs)
- [ ] [cli/src/commands/tasks/validate.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/tasks/validate.rs)
- [ ] [cli/src/commands/token/forgejo.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/token/forgejo.rs)
- [ ] [cli/src/commands/token/git_credential.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/token/git_credential.rs)
- [ ] [cli/src/commands/token/github.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/token/github.rs)
- [ ] [cli/src/commands/token/gitlab.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/token/gitlab.rs)
- [ ] [cli/src/commands/token/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/token/mod.rs)
- [ ] [cli/src/commands/tool_alias/get.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/tool_alias/get.rs)
- [ ] [cli/src/commands/tool_alias/ls.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/tool_alias/ls.rs)
- [ ] [cli/src/commands/tool_alias/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/tool_alias/mod.rs)
- [ ] [cli/src/commands/tool_alias/set.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/tool_alias/set.rs)
- [ ] [cli/src/commands/tool_alias/unset.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/tool_alias/unset.rs)
- [ ] [cli/src/commands/tool_stub.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/tool_stub.rs)
- [ ] [cli/src/commands/trust.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/trust.rs)
- [ ] [cli/src/commands/unset.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/unset.rs)
- [ ] [cli/src/commands/untrust.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/untrust.rs)
- [ ] [cli/src/commands/usage.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/usage.rs)
- [ ] [cli/src/commands/watch.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/watch.rs)

### 混合父模块不得整删

- [cli/src/commands/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/mod.rs)：删除排除variant、mod、dispatch、裸task和external回退；保留各独立职责和 D04 辅助命令；旧 migrate 本轮不扩建、不顺手删，其余 23 个核心入口接线。
- [cli/src/commands/project.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/project.rs)：只退出init/attach，保留ls/status与必要登记模型提取。
- [cli/src/commands/deps/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/deps/mod.rs)：保留install/add/remove。
- [cli/src/commands/installs/mod.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/installs/mod.rs)：ls/select 保留；migrate 本轮不扩建、不用于旧测试数据接管，现有入口不因数据清理擅自删除。
- [cli/src/commands/command_effects.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/command_effects.rs)：重新映射真实权限，不以“read可自动安装”的旧注释作为目标合同。
- [cli/src/commands/editor.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/editor.rs)：作为helper逐一核剩余消费者；不得单凭未在Commands中出现判为死代码。
- [cli/src/commands/shim.rs](/Users/fuxinao/文件/project/pakro/cli/src/commands/shim.rs)：内部运行接缝与D03有关，不能仅因公开reshim退出就机械删除。

## D-DOMAIN：非核心实现的完整文件边界

这些目录不是一次 `rm -rf` 的操作范围。R42先提取仍使用的基础能力，R43再按下列每个实际文件的引用情况退出。用户数据目录与源码目录完全不同，不得混用。


### src/core/daemons/（当前 10 个文件）

- [ ] [src/core/daemons/hook_env.rs](/Users/fuxinao/文件/project/pakro/src/core/daemons/hook_env.rs)
- [ ] [src/core/daemons/mod.rs](/Users/fuxinao/文件/project/pakro/src/core/daemons/mod.rs)
- [ ] [src/core/daemons/ports.rs](/Users/fuxinao/文件/project/pakro/src/core/daemons/ports.rs)
- [ ] [src/core/daemons/presets.rs](/Users/fuxinao/文件/project/pakro/src/core/daemons/presets.rs)
- [ ] [src/core/daemons/providers.rs](/Users/fuxinao/文件/project/pakro/src/core/daemons/providers.rs)
- [ ] [src/core/daemons/providers_nats.rs](/Users/fuxinao/文件/project/pakro/src/core/daemons/providers_nats.rs)
- [ ] [src/core/daemons/prune.rs](/Users/fuxinao/文件/project/pakro/src/core/daemons/prune.rs)
- [ ] [src/core/daemons/runtime.rs](/Users/fuxinao/文件/project/pakro/src/core/daemons/runtime.rs)
- [ ] [src/core/daemons/tasks.rs](/Users/fuxinao/文件/project/pakro/src/core/daemons/tasks.rs)
- [ ] [src/core/daemons/urls.rs](/Users/fuxinao/文件/project/pakro/src/core/daemons/urls.rs)

### runtimes/src/oci/（当前 10 个文件）

- [ ] [runtimes/src/oci/auth.rs](/Users/fuxinao/文件/project/pakro/runtimes/src/oci/auth.rs)
- [ ] [runtimes/src/oci/builder.rs](/Users/fuxinao/文件/project/pakro/runtimes/src/oci/builder.rs)
- [ ] [runtimes/src/oci/docker_archive.rs](/Users/fuxinao/文件/project/pakro/runtimes/src/oci/docker_archive.rs)
- [ ] [runtimes/src/oci/layer.rs](/Users/fuxinao/文件/project/pakro/runtimes/src/oci/layer.rs)
- [ ] [runtimes/src/oci/layer/cache.rs](/Users/fuxinao/文件/project/pakro/runtimes/src/oci/layer/cache.rs)
- [ ] [runtimes/src/oci/layout.rs](/Users/fuxinao/文件/project/pakro/runtimes/src/oci/layout.rs)
- [ ] [runtimes/src/oci/manifest.rs](/Users/fuxinao/文件/project/pakro/runtimes/src/oci/manifest.rs)
- [ ] [runtimes/src/oci/mod.rs](/Users/fuxinao/文件/project/pakro/runtimes/src/oci/mod.rs)
- [ ] [runtimes/src/oci/packages.rs](/Users/fuxinao/文件/project/pakro/runtimes/src/oci/packages.rs)
- [ ] [runtimes/src/oci/registry.rs](/Users/fuxinao/文件/project/pakro/runtimes/src/oci/registry.rs)

### src/core/task/（当前 33 个文件）

保留前置：remote_include、workspace声明发现、工程测试/生成依赖先解耦；任务专属调度/缓存可退出，但通用元数据缓存不退出。

- [ ] [src/core/task/deps.rs](/Users/fuxinao/文件/project/pakro/src/core/task/deps.rs)
- [ ] [src/core/task/mod.rs](/Users/fuxinao/文件/project/pakro/src/core/task/mod.rs)
- [ ] [src/core/task/task_cache.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_cache.rs)
- [ ] [src/core/task/task_cache_audit.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_cache_audit.rs)
- [ ] [src/core/task/task_cache_store.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_cache_store.rs)
- [ ] [src/core/task/task_confirm.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_confirm.rs)
- [ ] [src/core/task/task_context_builder.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_context_builder.rs)
- [ ] [src/core/task/task_dep.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_dep.rs)
- [ ] [src/core/task/task_executor.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_executor.rs)
- [ ] [src/core/task/task_fetcher.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_fetcher.rs)
- [ ] [src/core/task/task_file_providers/local_task.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_file_providers/local_task.rs)
- [ ] [src/core/task/task_file_providers/mod.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_file_providers/mod.rs)
- [ ] [src/core/task/task_file_providers/remote_task_git.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_file_providers/remote_task_git.rs)
- [ ] [src/core/task/task_file_providers/remote_task_http.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_file_providers/remote_task_http.rs)
- [ ] [src/core/task/task_file_providers/remote_task_oci.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_file_providers/remote_task_oci.rs)
- [ ] [src/core/task/task_helpers.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_helpers.rs)
- [ ] [src/core/task/task_list.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_list.rs)
- [ ] [src/core/task/task_load_context.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_load_context.rs)
- [ ] [src/core/task/task_output.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_output.rs)
- [ ] [src/core/task/task_output_handler.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_output_handler.rs)
- [ ] [src/core/task/task_results_display.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_results_display.rs)
- [ ] [src/core/task/task_scheduler.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_scheduler.rs)
- [ ] [src/core/task/task_script_parser.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_script_parser.rs)
- [ ] [src/core/task/task_source_checker.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_source_checker.rs)
- [ ] [src/core/task/task_sources.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_sources.rs)
- [ ] [src/core/task/task_template.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_template.rs)
- [ ] [src/core/task/task_tool_installer.rs](/Users/fuxinao/文件/project/pakro/src/core/task/task_tool_installer.rs)
- [ ] [src/core/task/workspace.rs](/Users/fuxinao/文件/project/pakro/src/core/task/workspace.rs)
- [ ] [src/core/task/workspace/cargo.rs](/Users/fuxinao/文件/project/pakro/src/core/task/workspace/cargo.rs)
- [ ] [src/core/task/workspace/git.rs](/Users/fuxinao/文件/project/pakro/src/core/task/workspace/git.rs)
- [ ] [src/core/task/workspace/go.rs](/Users/fuxinao/文件/project/pakro/src/core/task/workspace/go.rs)
- [ ] [src/core/task/workspace/node.rs](/Users/fuxinao/文件/project/pakro/src/core/task/workspace/node.rs)
- [ ] [src/core/task/workspace/uv.rs](/Users/fuxinao/文件/project/pakro/src/core/task/workspace/uv.rs)

### src/core/system/（当前 77 个文件）

保留前置：system::sudo、系统组件诊断、实际文件/进程原语，以及仍被配置使用的历史/文件逻辑先提取；不能整删。

- [ ] [src/core/system/accounts.rs](/Users/fuxinao/文件/project/pakro/src/core/system/accounts.rs)
- [ ] [src/core/system/accounts_non_linux.rs](/Users/fuxinao/文件/project/pakro/src/core/system/accounts_non_linux.rs)
- [ ] [src/core/system/compose.rs](/Users/fuxinao/文件/project/pakro/src/core/system/compose.rs)
- [ ] [src/core/system/defaults.rs](/Users/fuxinao/文件/project/pakro/src/core/system/defaults.rs)
- [ ] [src/core/system/defaults/dock.rs](/Users/fuxinao/文件/project/pakro/src/core/system/defaults/dock.rs)
- [ ] [src/core/system/deps.rs](/Users/fuxinao/文件/project/pakro/src/core/system/deps.rs)
- [ ] [src/core/system/dotfile_groups.rs](/Users/fuxinao/文件/project/pakro/src/core/system/dotfile_groups.rs)
- [ ] [src/core/system/driver.rs](/Users/fuxinao/文件/project/pakro/src/core/system/driver.rs)
- [ ] [src/core/system/edits.rs](/Users/fuxinao/文件/project/pakro/src/core/system/edits.rs)
- [ ] [src/core/system/files.rs](/Users/fuxinao/文件/project/pakro/src/core/system/files.rs)
- [ ] [src/core/system/firewall.rs](/Users/fuxinao/文件/project/pakro/src/core/system/firewall.rs)
- [ ] [src/core/system/firewall_non_linux.rs](/Users/fuxinao/文件/project/pakro/src/core/system/firewall_non_linux.rs)
- [ ] [src/core/system/history/checkpoint.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/checkpoint.rs)
- [ ] [src/core/system/history/checkpoint/preimages.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/checkpoint/preimages.rs)
- [ ] [src/core/system/history/config.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/config.rs)
- [ ] [src/core/system/history/describe_command.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/describe_command.rs)
- [ ] [src/core/system/history/enrollment.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/enrollment.rs)
- [ ] [src/core/system/history/health.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/health.rs)
- [ ] [src/core/system/history/journal.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/journal.rs)
- [ ] [src/core/system/history/manifest.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/manifest.rs)
- [ ] [src/core/system/history/mod.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/mod.rs)
- [ ] [src/core/system/history/notices.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/notices.rs)
- [ ] [src/core/system/history/notify.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/notify.rs)
- [ ] [src/core/system/history/notify/macos.m](/Users/fuxinao/文件/project/pakro/src/core/system/history/notify/macos.m)
- [ ] [src/core/system/history/notify/macos.plist](/Users/fuxinao/文件/project/pakro/src/core/system/history/notify/macos.plist)
- [ ] [src/core/system/history/notify/macos.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/notify/macos.rs)
- [ ] [src/core/system/history/recovery.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/recovery.rs)
- [ ] [src/core/system/history/replay.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/replay.rs)
- [ ] [src/core/system/history/scope.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/scope.rs)
- [ ] [src/core/system/history/select.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/select.rs)
- [ ] [src/core/system/history/shadow.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/shadow.rs)
- [ ] [src/core/system/history/store.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/store.rs)
- [ ] [src/core/system/history/sync/apply.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/sync/apply.rs)
- [ ] [src/core/system/history/sync/directories.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/sync/directories.rs)
- [ ] [src/core/system/history/sync/files.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/sync/files.rs)
- [ ] [src/core/system/history/sync/format.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/sync/format.rs)
- [ ] [src/core/system/history/sync/graph.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/sync/graph.rs)
- [ ] [src/core/system/history/sync/layout.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/sync/layout.rs)
- [ ] [src/core/system/history/sync/mod.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/sync/mod.rs)
- [ ] [src/core/system/history/sync/network.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/sync/network.rs)
- [ ] [src/core/system/history/sync/onboard.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/sync/onboard.rs)
- [ ] [src/core/system/history/sync/origin.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/sync/origin.rs)
- [ ] [src/core/system/history/sync/preflight.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/sync/preflight.rs)
- [ ] [src/core/system/history/sync/publish.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/sync/publish.rs)
- [ ] [src/core/system/history/sync/reconcile.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/sync/reconcile.rs)
- [ ] [src/core/system/history/sync/run.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/sync/run.rs)
- [ ] [src/core/system/history/sync/share.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/sync/share.rs)
- [ ] [src/core/system/history/sync/state.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/sync/state.rs)
- [ ] [src/core/system/history/tracked.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/tracked.rs)
- [ ] [src/core/system/history/watch/mod.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/watch/mod.rs)
- [ ] [src/core/system/history/watch/noise.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/watch/noise.rs)
- [ ] [src/core/system/history/watch/plan.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/watch/plan.rs)
- [ ] [src/core/system/history/watch/runtime.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/watch/runtime.rs)
- [ ] [src/core/system/history/watch/schedule.rs](/Users/fuxinao/文件/project/pakro/src/core/system/history/watch/schedule.rs)
- [ ] [src/core/system/hooks.rs](/Users/fuxinao/文件/project/pakro/src/core/system/hooks.rs)
- [ ] [src/core/system/launchd.rs](/Users/fuxinao/文件/project/pakro/src/core/system/launchd.rs)
- [ ] [src/core/system/login_shell.rs](/Users/fuxinao/文件/project/pakro/src/core/system/login_shell.rs)
- [ ] [src/core/system/managed_files.rs](/Users/fuxinao/文件/project/pakro/src/core/system/managed_files.rs)
- [ ] [src/core/system/mod.rs](/Users/fuxinao/文件/project/pakro/src/core/system/mod.rs)
- [ ] [src/core/system/remote.rs](/Users/fuxinao/文件/project/pakro/src/core/system/remote.rs)
- [ ] [src/core/system/remote_repository.rs](/Users/fuxinao/文件/project/pakro/src/core/system/remote_repository.rs)
- [ ] [src/core/system/remote_testdata/pakro-v2026.8.1-SHASUMS256.txt](/Users/fuxinao/文件/project/pakro/src/core/system/remote_testdata/pakro-v2026.8.1-SHASUMS256.txt)
- [ ] [src/core/system/remote_testdata/pakro-v2026.8.1-SHASUMS256.txt.minisig](/Users/fuxinao/文件/project/pakro/src/core/system/remote_testdata/pakro-v2026.8.1-SHASUMS256.txt.minisig)
- [ ] [src/core/system/repos.rs](/Users/fuxinao/文件/project/pakro/src/core/system/repos.rs)
- [ ] [src/core/system/resources.rs](/Users/fuxinao/文件/project/pakro/src/core/system/resources.rs)
- [ ] [src/core/system/scheduled_tasks.rs](/Users/fuxinao/文件/project/pakro/src/core/system/scheduled_tasks.rs)
- [ ] [src/core/system/secrets.rs](/Users/fuxinao/文件/project/pakro/src/core/system/secrets.rs)
- [ ] [src/core/system/service_exec.rs](/Users/fuxinao/文件/project/pakro/src/core/system/service_exec.rs)
- [ ] [src/core/system/services.rs](/Users/fuxinao/文件/project/pakro/src/core/system/services.rs)
- [ ] [src/core/system/services_common.rs](/Users/fuxinao/文件/project/pakro/src/core/system/services_common.rs)
- [ ] [src/core/system/services_non_linux.rs](/Users/fuxinao/文件/project/pakro/src/core/system/services_non_linux.rs)
- [ ] [src/core/system/shell_activation.rs](/Users/fuxinao/文件/project/pakro/src/core/system/shell_activation.rs)
- [ ] [src/core/system/sudo.rs](/Users/fuxinao/文件/project/pakro/src/core/system/sudo.rs)
- [ ] [src/core/system/systemd.rs](/Users/fuxinao/文件/project/pakro/src/core/system/systemd.rs)
- [ ] [src/core/system/templating.rs](/Users/fuxinao/文件/project/pakro/src/core/system/templating.rs)
- [ ] [src/core/system/unapply.rs](/Users/fuxinao/文件/project/pakro/src/core/system/unapply.rs)
- [ ] [src/core/system/user_services.rs](/Users/fuxinao/文件/project/pakro/src/core/system/user_services.rs)

### packages/system/（当前 37 个文件）

保留前置：区分宿主包驱动与源码构建需要的组件事实。Brew原生下载/签名/relocation若仍有明确核心消费者，保留或提取唯一实现。

- [ ] [packages/system/apk.rs](/Users/fuxinao/文件/project/pakro/packages/system/apk.rs)
- [ ] [packages/system/apt.rs](/Users/fuxinao/文件/project/pakro/packages/system/apt.rs)
- [ ] [packages/system/aur.rs](/Users/fuxinao/文件/project/pakro/packages/system/aur.rs)
- [ ] [packages/system/brew/api.rs](/Users/fuxinao/文件/project/pakro/packages/system/brew/api.rs)
- [ ] [packages/system/brew/cask/app_version.rs](/Users/fuxinao/文件/project/pakro/packages/system/brew/cask/app_version.rs)
- [ ] [packages/system/brew/cask/artifacts.rs](/Users/fuxinao/文件/project/pakro/packages/system/brew/cask/artifacts.rs)
- [ ] [packages/system/brew/cask/bulk.rs](/Users/fuxinao/文件/project/pakro/packages/system/brew/cask/bulk.rs)
- [ ] [packages/system/brew/cask/fetch.rs](/Users/fuxinao/文件/project/pakro/packages/system/brew/cask/fetch.rs)
- [ ] [packages/system/brew/cask/flight.rs](/Users/fuxinao/文件/project/pakro/packages/system/brew/cask/flight.rs)
- [ ] [packages/system/brew/cask/mod.rs](/Users/fuxinao/文件/project/pakro/packages/system/brew/cask/mod.rs)
- [ ] [packages/system/brew/cask/paths.rs](/Users/fuxinao/文件/project/pakro/packages/system/brew/cask/paths.rs)
- [ ] [packages/system/brew/cask/running.rs](/Users/fuxinao/文件/project/pakro/packages/system/brew/cask/running.rs)
- [ ] [packages/system/brew/cask/state.rs](/Users/fuxinao/文件/project/pakro/packages/system/brew/cask/state.rs)
- [ ] [packages/system/brew/cask/tests.rs](/Users/fuxinao/文件/project/pakro/packages/system/brew/cask/tests.rs)
- [ ] [packages/system/brew/cask_shim.rb](/Users/fuxinao/文件/project/pakro/packages/system/brew/cask_shim.rb)
- [ ] [packages/system/brew/fetch.rs](/Users/fuxinao/文件/project/pakro/packages/system/brew/fetch.rs)
- [ ] [packages/system/brew/maintenance.rs](/Users/fuxinao/文件/project/pakro/packages/system/brew/maintenance.rs)
- [ ] [packages/system/brew/mod.rs](/Users/fuxinao/文件/project/pakro/packages/system/brew/mod.rs)
- [ ] [packages/system/brew/pour.rs](/Users/fuxinao/文件/project/pakro/packages/system/brew/pour.rs)
- [ ] [packages/system/brew/prefix.rs](/Users/fuxinao/文件/project/pakro/packages/system/brew/prefix.rs)
- [ ] [packages/system/brew/relocate.rs](/Users/fuxinao/文件/project/pakro/packages/system/brew/relocate.rs)
- [ ] [packages/system/brew/resolve.rs](/Users/fuxinao/文件/project/pakro/packages/system/brew/resolve.rs)
- [ ] [packages/system/brew/shim.rb](/Users/fuxinao/文件/project/pakro/packages/system/brew/shim.rb)
- [ ] [packages/system/brew/source.rs](/Users/fuxinao/文件/project/pakro/packages/system/brew/source.rs)
- [ ] [packages/system/brew/tag.rs](/Users/fuxinao/文件/project/pakro/packages/system/brew/tag.rs)
- [ ] [packages/system/brew/tap.rs](/Users/fuxinao/文件/project/pakro/packages/system/brew/tap.rs)
- [ ] [packages/system/brew/tap_cask_metadata.rb](/Users/fuxinao/文件/project/pakro/packages/system/brew/tap_cask_metadata.rb)
- [ ] [packages/system/brew/tap_formula_metadata.rb](/Users/fuxinao/文件/project/pakro/packages/system/brew/tap_formula_metadata.rb)
- [ ] [packages/system/dnf.rs](/Users/fuxinao/文件/project/pakro/packages/system/dnf.rs)
- [ ] [packages/system/flatpak.rs](/Users/fuxinao/文件/project/pakro/packages/system/flatpak.rs)
- [ ] [packages/system/mas.rs](/Users/fuxinao/文件/project/pakro/packages/system/mas.rs)
- [ ] [packages/system/mod.rs](/Users/fuxinao/文件/project/pakro/packages/system/mod.rs)
- [ ] [packages/system/nix.rs](/Users/fuxinao/文件/project/pakro/packages/system/nix.rs)
- [ ] [packages/system/pacman.rs](/Users/fuxinao/文件/project/pakro/packages/system/pacman.rs)
- [ ] [packages/system/scoop.rs](/Users/fuxinao/文件/project/pakro/packages/system/scoop.rs)
- [ ] [packages/system/winget.rs](/Users/fuxinao/文件/project/pakro/packages/system/winget.rs)
- [ ] [packages/system/zypper.rs](/Users/fuxinao/文件/project/pakro/packages/system/zypper.rs)

### src/crates/pakro-bootstrap/（当前 7 个文件）

- [ ] [src/crates/pakro-bootstrap/Cargo.toml](/Users/fuxinao/文件/project/pakro/src/crates/pakro-bootstrap/Cargo.toml)
- [ ] [src/crates/pakro-bootstrap/README.md](/Users/fuxinao/文件/project/pakro/src/crates/pakro-bootstrap/README.md)
- [ ] [src/crates/pakro-bootstrap/src/lib.rs](/Users/fuxinao/文件/project/pakro/src/crates/pakro-bootstrap/src/lib.rs)
- [ ] [src/crates/pakro-bootstrap/src/plan.rs](/Users/fuxinao/文件/project/pakro/src/crates/pakro-bootstrap/src/plan.rs)
- [ ] [src/crates/pakro-bootstrap/src/policy.rs](/Users/fuxinao/文件/project/pakro/src/crates/pakro-bootstrap/src/policy.rs)
- [ ] [src/crates/pakro-bootstrap/src/resource.rs](/Users/fuxinao/文件/project/pakro/src/crates/pakro-bootstrap/src/resource.rs)
- [ ] [src/crates/pakro-bootstrap/src/state.rs](/Users/fuxinao/文件/project/pakro/src/crates/pakro-bootstrap/src/state.rs)

### packages/crates/pakro-brew-metadata/（当前 12 个文件）

- [ ] [packages/crates/pakro-brew-metadata/Cargo.toml](/Users/fuxinao/文件/project/pakro/packages/crates/pakro-brew-metadata/Cargo.toml)
- [ ] [packages/crates/pakro-brew-metadata/README.md](/Users/fuxinao/文件/project/pakro/packages/crates/pakro-brew-metadata/README.md)
- [ ] [packages/crates/pakro-brew-metadata/src/cask/artifacts.rs](/Users/fuxinao/文件/project/pakro/packages/crates/pakro-brew-metadata/src/cask/artifacts.rs)
- [ ] [packages/crates/pakro-brew-metadata/src/cask/artifacts/ordinary.rs](/Users/fuxinao/文件/project/pakro/packages/crates/pakro-brew-metadata/src/cask/artifacts/ordinary.rs)
- [ ] [packages/crates/pakro-brew-metadata/src/cask/flight/mod.rs](/Users/fuxinao/文件/project/pakro/packages/crates/pakro-brew-metadata/src/cask/flight/mod.rs)
- [ ] [packages/crates/pakro-brew-metadata/src/cask/flight/path.rs](/Users/fuxinao/文件/project/pakro/packages/crates/pakro-brew-metadata/src/cask/flight/path.rs)
- [ ] [packages/crates/pakro-brew-metadata/src/cask/flight/step.rs](/Users/fuxinao/文件/project/pakro/packages/crates/pakro-brew-metadata/src/cask/flight/step.rs)
- [ ] [packages/crates/pakro-brew-metadata/src/cask/helpers.rs](/Users/fuxinao/文件/project/pakro/packages/crates/pakro-brew-metadata/src/cask/helpers.rs)
- [ ] [packages/crates/pakro-brew-metadata/src/cask/mod.rs](/Users/fuxinao/文件/project/pakro/packages/crates/pakro-brew-metadata/src/cask/mod.rs)
- [ ] [packages/crates/pakro-brew-metadata/src/cask/model.rs](/Users/fuxinao/文件/project/pakro/packages/crates/pakro-brew-metadata/src/cask/model.rs)
- [ ] [packages/crates/pakro-brew-metadata/src/cask/types.rs](/Users/fuxinao/文件/project/pakro/packages/crates/pakro-brew-metadata/src/cask/types.rs)
- [ ] [packages/crates/pakro-brew-metadata/src/lib.rs](/Users/fuxinao/文件/project/pakro/packages/crates/pakro-brew-metadata/src/lib.rs)

### packages/crates/pakro-brew-relocation/（当前 7 个文件）

- [ ] [packages/crates/pakro-brew-relocation/Cargo.toml](/Users/fuxinao/文件/project/pakro/packages/crates/pakro-brew-relocation/Cargo.toml)
- [ ] [packages/crates/pakro-brew-relocation/README.md](/Users/fuxinao/文件/project/pakro/packages/crates/pakro-brew-relocation/README.md)
- [ ] [packages/crates/pakro-brew-relocation/src/elf.rs](/Users/fuxinao/文件/project/pakro/packages/crates/pakro-brew-relocation/src/elf.rs)
- [ ] [packages/crates/pakro-brew-relocation/src/elf/tests.rs](/Users/fuxinao/文件/project/pakro/packages/crates/pakro-brew-relocation/src/elf/tests.rs)
- [ ] [packages/crates/pakro-brew-relocation/src/lib.rs](/Users/fuxinao/文件/project/pakro/packages/crates/pakro-brew-relocation/src/lib.rs)
- [ ] [packages/crates/pakro-brew-relocation/src/macho.rs](/Users/fuxinao/文件/project/pakro/packages/crates/pakro-brew-relocation/src/macho.rs)
- [ ] [packages/crates/pakro-brew-relocation/src/tests.rs](/Users/fuxinao/文件/project/pakro/packages/crates/pakro-brew-relocation/src/tests.rs)

## D-CARGO：依赖、构建与资源同步

| 位置/依赖 | 裁剪条件与保护 |
| --- | --- |
| Cargo.toml / Cargo.lock | 依照实际剩余使用删除，不顺便升级；workspace成员、path依赖、features转发和package include同时核对。 |
| rmcp | MCP产品退出且全树无消费者后删除。 |
| pakro-bootstrap、pakro-brew-metadata、pakro-brew-relocation | R42核心消费者迁出/保留后逐项决定；不是看到system就整删。 |
| pakro-cache-core | util/settings也使用，TLS feature也转发；删任务缓存不等于删此crate。 |
| pakro-sigstore / minisign / provenance | 仍服务制品校验，不能随自更新/插件CLI退出而删除。 |
| nix crate | Unix系统调用库，不是Nix宿主包管理器；锁/目录/进程仍需。 |
| AWS、S3、Packslip/Aqua来源 | 按保留的语言/包来源能力决定；不能用包名猜用途。 |
| notify/age/SOPS/plist/workspace依赖 | 对保留的配置/文件能力逐项查消费者，不整组删除。 |
| self_update 默认 feature | D04 已确认保留，不得通过依赖清理间接删除；发行身份/密钥未配置仍如实报错。 |
| src/build/main.rs | 只退出通知helper、daemon presets等已失去消费者的生成；registry/aqua生成保留。 |
| .config/settings.toml / src/schema / render-schema example | 核心配置与未知排除字段诊断同步；task schema退出先改生成器，不能只删产物。 |
| cli/generated、completions、man | R41同一规格生成核产生，不能手写另一份命令树。 |
| .config/tasks.toml / tests/tasks / .github/workflows | 先改直接工程执行方式再删产品run/exec；CI行为变更由用户发起验证，不代跑。 |
| packaging / LICENSE / vendor | 工程支持、真实许可证和第三方身份保留；只按实际失效引用收缩。 |
| .codex / .idea / .git / 用户文件 | 不在本次代码删除计划范围。 |

## 测试和夹具的处置

- 保留底层算法、真实FS/HTTP、身份/来源/预算/锁/恢复等独立价值测试。
- 旧测试若断言的是sidecar、项目复制或调用pip等旧行为，改为新架构对应的业务事实；不能继续保持旧错误行为求绿。
- 核心查询测试若使用dummy ASDF、project init/attach或exec准备数据，先换受控原生fixture，再删除旧入口；不因准备方式旧就删查询断言。
- runtime_snapshot中的 containment/变化检测等安全断言迁到共享owner；“必须复制独立整套解释器”的产品断言退出，不把制品snapshot也删掉。
- 共享安装/构建使用实际解释器，不用返回成功 JSON 的脚本冒充。IDE/外部运行的真实子进程保护随 R20 后置，其现有启动/监督资产不提前机械删除。
- live测试中写死的 /private/tmp 旧路径不能当新环境前提。R00改为由用户指定现有真实解释器，测试失败不得改用户原解释器。

## 决策保护清单

- D01：install_into 已取消，进入 D-CLI 退出清单。
- D02：旧 Pakro 测试数据按 R39 清理，不新增迁移代码；installs migrate 本轮不扩建，也不借清数据擅自删除现有命令。
- D03：IDE/外部直接运行接入后置，不阻塞当前共享安装和项目引用清理。
- D04：`cli/src/commands/cache/{mod,clear,prune,path}.rs`、`doctor/`、`test_tool.rs`、`env.rs`、`installs/select.rs`、`version.rs`、`self_update.rs`、`implode.rs` 明确保留，按 R43 改接统一边界；cache task 和任务专属分支仍排除。
- D05：Python → Java → Node，顺序不变。
- D06：默认常用包自动安装退出；显式 prune 允许清理无项目直接/间接引用的手工提前安装，历史安装记录不构成永久保留根。

## 每项实际删除时的记录格式

| 项目 | 必填内容 |
| --- | --- |
| 旧文件/函数 | 精确路径、函数和本次实际删除范围 |
| 替代位置 | 实际被调用的新/保留函数，不只写目录名 |
| 剩余消费者 | 生产/测试/生成链逐项核对结果 |
| 用户验证 | 测什么的大白话、实际命令、运行到的用例数、结果 |
| 数据影响 | 明确仅代码；触及用户数据时须核对现有授权和准确路径；D02 已批准的无用旧测试数据按 R39 清理，不扩大范围 |
| 状态 | 未开始 / 已接线待测 / 用户通过可退役 / 已删除并复验 |

