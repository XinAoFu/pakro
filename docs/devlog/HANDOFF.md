# 重构执行交接 — 2026-10-19（Asia/Shanghai，F1 RED 第一轮已读、夹具已修）

## 下一步

R00–R06已完成并勾选。**2026-10-18用户明确下令开始F1**；F1第一步"测试已写并注册"已完成（见[2026-10-18日志](log/2026-10-18-10-F1测试先行.md)）。

**2026-10-19用户运行了第一轮 RED（`target/f1-manual-20261019-104029.log` 为 --list 编译通过、5 名注册；`104202.log` 为 4 项全 FAILED）**：`f1_incomplete_version_has_no_effects` 是正确 RED（桩在 3.11.14 处拒绝）；其余 3 项因夹具把未规范化 tempdir 当数据根、撞上 pakro-store 不跟随链接的根打开而 ENOTDIR，失败地点错误，不算 RED。已修复（三用例 canonicalize 数据根；f-manual-check.sh 改为逐段记录、任一失败 exit 1），详见[2026-10-19日志](log/2026-10-19-1-F1-RED确认与夹具修复.md)。

**2026-10-19第二轮运行（`target/f1-manual-20261019-104520.log`）：单测 RED 全部确认**——4 项均以 "f1 ... is not implemented" 形态失败（exact_version 拒 3.11.14、查询到 selection 桩、selection 两项到 location 绑定桩）。e2e 因隔离 workdir 无 Cargo.toml 失败（非 RED，已改为子 shell `cd "$ROOT"` 跑 cargo）；格式检查 5 处差异已按 rustfmt 手工修正。详见[2026-10-19日志](log/2026-10-19-1-F1-RED确认与夹具修复.md)。

**2026-10-19第四轮运行（`target/f1-manual-20261019-105327.log`）：单测 RED 稳定、格式通过、暂存助手首次真实执行并通过**（`1 passed` 并打印 staged 行）。随后暴露布局错位：暂存曾借 R06 夹具 `operation_context` 多拼一层 `data`，库落在 `$PAKRO_DATA_DIR/data/languages`；产品契约是 `$PAKRO_DATA_DIR/languages`。已改为暂存助手直接 `capture_operation_context`（data_root=暂存根本身、cache=`.f1-stage-cache`）。详见[2026-10-19日志](log/2026-10-19-1-F1-RED确认与夹具修复.md)。

**2026-10-19 13:30 最小实现第一块完成（库层+程序层）**：exact_version/location/identity 实现；model 增 language 选择槽与 projects 登记（codec schema3，兼容读 schema2）；语言库 enumerate_published/acquire_by_key 桥（manifest 重建 spec，Corrupt/Unreadable 分流）；project repository（数据根 projects/ R05 store，CAS 单 catalog）+read_views；commit（项目EX→语言SH复核→reload→登记+仅语言选择→单次CAS）；query select_existing/paths 实现。**未改 CLI**（hook_env/env/toolset 属下一块）；有意未动 catalog.rs/prefix/control.rs（见日志缩减记录）。未运行编译/测试。

**2026-10-19 16:30 F1 最小实现完成、套件全绿（2026-10-19用户授权Agent自测≤10分钟/组）。**`target/f1-manual-20261019-121806.log`：单测 4/4、e2e 全链路（暂存→A/B同payload→缺项不下载→退出恢复→--force 新进程读已提交选择）、格式全部通过。CLI 接线：`programs/directory.rs` 唯一 pub 入口（幂等提交/只读两模式）；hook_env 前插 tool_paths；env 只读化并移出 python 隐式安装。相邻回归全绿（model/catalog/prefix/selections/journal/language + env、activate e2e）。**既有失败报告：`cli/test_activate_export` 在 F1 改动前即失败**（`__PAKRO_EXE=/` 断言与单引号输出不匹配，git stash 对照证实），待用户裁决。`target/debug`（23G）已按用户指示清理，日志与 retained-build-logs 保留。

**2026-10-19 20:10 F1 全部完成（五步：测试注册→RED→最小实现→用户验收→两轴审查）。**用户以真实终端演示确认四项验收行为；双轴审查的唯一阻塞（提交锁序与规格相反：语言SH原在prepare先于项目EX）已修复为 项目EX→语言SH→CAS，另修 env.rs 生产 unwrap 与 directory.rs 静默吞错；处置后 `target/f1-manual-20261019-200450.log` 全绿+model/language 回归通过。非阻塞建议记 F2+（AlreadyApplied 并发收敛、具名常量、EEXIST 回退等，见日志 20:10）。既有失败 `cli/test_activate_export` 仍待用户裁决（先于F1存在）。**2026-10-19 21:30 F2 已开始（用户下令+已提交 c5f4457f9）。**F2 第一步完成：四处测试+可编译桩已注册并自测 RED（`cargo test f2_` 4/4 失败于桩，格式通过）；`f-manual-check.sh` 增 f2 模式（离线批），e2e `test_f2_exact_python_install` 需网络属用户道。**2026-10-19 22:30 F2 最小实现第一块完成：离线核全绿**（`f2_` 4/4、F1 套件复跑通过 `target/f1-manual-20261019-205546.log`、language 38/38）。已实现 choose_unique_release（多 build 报歧义）、reject_legacy_mutation（共享库 canonicalize 包含判定）、install_exact_python（枚举复用零获取/acquire→spec_from_artifact→prepare_language/项目接 F1 关联）；修 enumerate 缺库=空集。
**2026-10-19 22:15 F2 ①②③接线完成，离线全绿**（f2 批 4/4、f1 套件 `target/f1-manual-20261019-213620.log`、格式；离线 D10 冒烟两种写法均副作用前拒绝）。CLI 双写法快速路径+单工具 install_runtimes 复用+成功后 F1 关联；install_native_body 提取、精确请求走 choose_unique_release；拒绝门 5 处（backend purge/repair/uninstall_impl、resolver purge_installs、purgatory schedule），门签名改 data_root；无参数安装留旧管线、rollback/system_install 门为记录的后续加固点（详见日志 22:15）。
**2026-10-19 23:55 用户指令：F3+F4 连续实施、全部测试推迟到 F4 后统一执行（期间仅编译检查）。**F3 第一批已落：包库骨架 blob/tree/manifest/layouts（+2 测试）、shared_runtime 桥（+1 测试）、package_graph 投影合同、programs/install/resolve（+1 测试）——已注册、编译通过、未执行。F3 剩余清单见日志 23:55 条目（真实闭包接线、R05/wheel 核接线、layouts 前缀+启动适配、图持久化、目录选择包投影、CLI install/deps 分流、plan/layout/verify）。
**F2 剩余：用户道网络 e2e** `cd '/Users/fuxinao/文件/project/pakro' && tests/e2e/run_test tests/e2e/cli/test_f2_exact_python_install`（真实下载 PBS 3.13.16 约 25MB）；通过后用户真实场景验收+两轴审查。

```bash
cd '/Users/fuxinao/文件/project/pakro'
bash tests/scripts/f-manual-check.sh f1       # 日志存 target/f1-manual-*.log
```

另：上次失败的 e2e 在 `/private/var/folders/.../test_f1_directory_selection.*` 遗留一个隔离目录，确认无用后可手动删除。

## F1 当前状态

- 已注册：`programs::shared::exact_version::tests::f1_incomplete_version_has_no_effects`、`programs::query::paths::tests::f1_local_selection_reports_missing_and_ambiguity`、`programs::install::selection::tests::{f1_language_only_selection_survives_restart,f1_failed_selection_keeps_old_references}`、e2e `tests/e2e/cli/test_f1_directory_selection`（经 `#[ignore] f1_e2e_stage_shared_language` 离线暂存夹具语言，无网络）。
- 桩接缝：query/{snapshot,paths}、install/selection、shared/{exact_version,commit}、libraries/project/{identity,location,repository,read_views}；native.rs `is_exact_request` 提为 pub(crate)；repository/tests.rs 夹具参数化。
- 实现期钉住的两个契约（改布局须同步测试并记日志）：项目 catalog 在数据根 `projects/` R05 store（`.control/catalog`）；hook 成功后把 payload `bin` 目录加入 PATH。
- 未做（属最小实现步骤）：hook_env/env/toolset/shims 接线、model/codec 仅语言选择表示、语言库列举桥；未删旧代码、未提交Git。

## R06 之前的交接（历史保留）

R00–R06已完成并勾选。用户明确要求**先不要开启R07**，最新再次要求**只更新架构和PLAN，不改代码、不开始实现**。后续编译、测试和检查仍由用户手动执行；本次只修改文档，没有运行编译/测试/fmt/check，也没有提交Git。

最新工作是**按用户要求把F1–F4补成逐文件实施详案，仍未实施**。PLAN每阶段已列现有修改位置、拟新增文件、默认只复用资产、输入输出、实施动作、测试文件/行为和禁止范围；恢复旧详案的明确程度，没有恢复45项全量前置。未来F阶段手动脚本/用例清单也只是计划，当前不存在、不能执行。详见[逐文件计划细化记录](log/2026-10-18-9-首版逐文件计划细化.md)。

静态源码核对所得关键接缝已写入PLAN：F1补本地版本到R06实际实例的查询桥及同catalog的仅语言选择/持久登记，不伪造layout receipt；F2在CLI副作用前检查完整版本，并纠正native按最新build选发行；F3将OwnedRuntimeSnapshot/整套复制改为真实LanguageLease，持久化完整包依赖边；F4不能用项目读取错误的空列表、扁平引用或暂存cleanup权限来决定正式对象删除。现有parser/solver/IO和未迁移消费者继续保留，不创建第二套实现。

既定范围为**D09首版增加F4、D10重申完整精确版本，未实施**。当前PLAN为四阶段：F1进目录选择库内已有Python/缺项提示；F2显式安装缺项；F3真实wheel及A/B共享、包选择隔离；F4查询已知项目、库内语言/包及其关联，删除指定语言/包。有项目或依赖在用就列明并拒绝，只有确认无引用才允许删除；批量prune、force/原位修改和完整回收矩阵仍后置。首版范围仍限当前macOS arm64/PBS CPython、单公开来源兼容wheel，长期Python→Java→Node顺序保留。

D09要求F1–F3同步维护最低持久精确关联及真实依赖边，供F4列出“谁用了什么”；成功选用时内部登记，不增加用户步骤，不让只读查询偷偷写库。退出目录不释放持久项目引用；新选择成功后才更新，失败、损坏或暂不可访问不当成零引用。指定删除与新增引用使用同一对象保护，取得排他保护后复核最新引用和对象身份；活跃内部操作、无法确认的关联也不能放行，不递归删除其他对象。外部进程完整生命周期仍按D03后置。

D10适用于用户下载请求和项目直接清单：Python须完整补丁号，例如3.11.14，不能用3.11、3.11.xx、通配、范围、latest/浮动别名自动补齐；命令可读取配置里的完整版本，两处都未明确就先报错，不能先获取远端或写入。包沿用真实上游完整发行号，不机械补零。包作者传递依赖范围仍真实求解，各次实际制品下载先确定精确版本/制品并保存结果；完整可移植锁后置不等于精确记录后置。平台/build/来源/摘要仍决定实际身份，多实例歧义不任取。现存解析器和旧CLI尚未接通这些新约束，不声称已实现。

原详案逐字节保存在`docs/devlog/archive/PLAN-R00-R45-2026-10-18.md`，仅供文件/合同/历史状态查阅，不再作为必须逐项完成的前置队列，也不恢复历史测试授权。此前从3236行缩为115行三阶段提案是历史计划审查结果，现已按本次指令扩为F1–F4；不再据旧提案把所有物理删除或本地包查询后移。不要自行执行旧R07或任何F阶段。最新记录见[首版查询删除与精确版本](log/2026-10-18-8-首版查询删除与精确版本.md)。

D08取消独立离线模式的决定保持：已有直接用、缺项由用户决定下载、没网明确报错，来源和凭证校验保留。历史R10改为统一本地复用和授权获取，关联R16/R19、Java阶段/R38、R40/R45的未来离线要求及计划测试名已同步。内部只读/worker隔离仍有效，现存offline代码字段、旧测试及R01/R06历史记录没有删除或重写。

D07项目使用方式同样仅文档、未实施：进入项目目录→读配置→找库中已安装语言/包→直接选用；缺项由用户决定是否下载，允许必要内部适配但不增加操作步骤。内部SharedLayout/启动配置不作为用户必须创建/激活的环境。R06共享库基础和历史通过证据保持，不等于D07/D08接线已经完成。

原详案已调整R12/R13内部适配、R16明确安装触发、R18/R26目录选择、R20基础选择与完整生命周期后置边界、R43隐式补装与必要hook保留及R45验收；当前F计划从中取必要部分。FILES对activate/deactivate/hook_env改为必要部分待拆分保留，不恢复通用Shell/task/daemon。具体Python包加载机制仍须在对应任务细化，不能自选为venv或只改PYTHONPATH。不自动开始任何实现任务。

R06没有剩余待执行验证。`tests/scripts/r06-manual-check.sh`的all/--remaining/--format-only入口保留供未来必要复现，当前不应无故重复运行。旧顺序下一项是R07；现在按F1–F4安排交付，但用户仅授权改文档，不能按任何一条自行开工。

完成证据：180052.log的主库84/84 + 182044.log的Sigstore4/4、util5/5、相邻回归19+20+10+15+8全部通过；按完整测试名去重150项（pakro133、sigstore4、util13），重复16项、0 tests及嵌套helper不累计。三包cargo check实际Finished dev 44.53秒。最终182740.log为--format-only，成功标记仅在37文件格式检查与git diff --check均成功后打印，已确认格式收尾通过。原pipeline排版失败已关闭。

此前两项Sigstore HTTP前置失败已由用户续跑验证修复：测试子进程NO_PROXY隔离及新真实loopback用例均通过；生产client/系统设置与密码算法未改。大量既有unused/dead-code/future提示仍在，编译有Finished，不扩R06去清理后续模块。

历史清理记录：用户授权后于16:29删除target编译产物，实测可用空间增加54.59GB，当时target剩余约9.3MB；源码、未提交修改、74份测试日志及脚本保留，506份内部编译输出在target/retained-build-logs-20261018。之后用户已手动重新构建，因此9.3MB不代表当前占用。明细为target/r06-cleanup-result.json，外部真实归档未删除，本次未再次清理。

## R06 已完成的接续接口

- 先读 `log/2026-10-18-4-R06共享语言库.md` 和历史详案 `archive/PLAN-R00-R45-2026-10-18.md` 的R06记录；历史详案不作为当前执行队列。新增language repository/manifest/lease/publication、program/runtime；复用R05 IO/Flock/CAS、原native plan/VerifiedArtifact、原probe/compatibility核。
- 原prefix/verify参数化为唯一held-tree walk/hash，支持受管内部链接；原v1 prefix receipt保留。PythonRuntimeHandle新增Arc RuntimeProtection，实际SH和deadline前后检查。
- owner tracked-content、父目录移走、累计预算、cleanup、CAS错误保留、manifest stdlib/identity/权限、PyPy无效patch、SH保活、bridge和异步取消均已实际GREEN。全部历史RED/编译/夹具前置分类见日志，不把重复运行累加成新用例。
- 官方归档mode去022、冻结cache暂存、cap SHA256、已有SH下来源重验、policy前置及等价官方locator已通过用户主库测试。新SHA memo域独立frozen-sha256的util回归也已在182044.log通过。
- 新Existing/首次安装调用`native::provenance::{cached,record}`，持有并复核冻结C根及三级目录；原deadline/取消、600/nlink1、64KiB预算、无覆盖及损坏保留。只读缺失不创建，写权限沿libraries；旧proof字节/文件名核与旧consumer保留。14项proof测试、相邻回归、非测试编译及最终格式/diff检查均已通过。
- 旧任意Path extract/remove_all已删除，旧11条extract测试迁到同owner核；Unix native安装返回前不会再走旧默认包/venv后处理。冻结Settings贯穿来源/缓存/transport；不代表后续R07/R16全部接线完成。
- 用户已指定并授权下载至`/Users/fuxinao/文件/project/pakro_test`；原归档为`cpython-3.13.16+20261013-aarch64-apple-darwin-install_only_stripped.tar.gz`，SHA256 `16a5332fb0dcb77f163fc8f8cbccd95be1644cc53e68e5d2a79f50afd6680a0e`，25268458字节。首次实际验收因mode失败；修复后两项在最近完成组中真实通过，75.33秒。关闭旧租约后新进程复用，握手并发只一次获取且同root/stdlib inode。范围为库门面，不是完整项目选择/CLI。
- 清理前历史空间审计：target文件名合计75.69GB，唯一inode约55.36GB内容/55.58GB分配块，31112个硬链接路径解释统计差异；缺开工基线，不能全算本轮新增。用户随后授权的清理已完成，见上方16:29记录；此次代码补齐没有重建编译目录。

## R05 已完成的接续接口

- `pakro-store::control::{ReadStore,StoreWriter}`：同一授权绝对根词法规范化，不跟随symlink、不接受父级组件或按项目fallback。ReadStore缺失/空根返回None，不创建且不暴露writer/raw File；StoreWriter仅初始化空库。旧prefix保留ASCII根要求，共用根支持Unicode/raw路径。
- `.allocation.lock`和`.leases/build`在初始化时建立；`.control/.identity`以PKCTRL01记录9个inode对，共152字节。已有marker下缺锁/缺目录/替换均报错，不修复；并发initializer窗口等待同一原deadline。初始化中断保留证据，不自动修复。
- `StableLock`固定parent/name/inode，独立fresh-open Flock；Shared是O_RDONLY，drop不unlink。`ObjectLease`克隆保活同一实际guard。`OrderedLocks`按项目→稳定key排序对象→catalog，禁升级、角色别名和catalog后新增对象，等待后复核先前保护。原始batch和累计保活集合都受LockLimits约束（默认1024对象/1MiB key），原deadline不续期。
- `ReadControl/WriterControl`惰性进入catalog SH/EX，metadata操作保持保护直到scope结束；对象锁核验本库build目录和`<key>.lock`，拒绝跨库或错key。bounded reads/listing/CAS/fsync和no-follow/UID/mode/nlink/inode在唯一内核；writer隐藏句柄仅用于旧应用安全接缝。
- prefix显式`open_writer(...,deadline)`；旧无deadline `open`仅测试fixture。分配先拒绝已有g，再持久化StableLock、取得build EX、再catalog；等待后持续复核。创建/write/flush/finish和discard捕获/逐项删除使用同一build pin与原deadline，失效保留证据。对象锁不因目录删除而清理。
- 原control薄层、scan、ownership读写和恢复已真调用新核；恢复先对象保护再读catalog/journal，不倒序。cleanup沿用同一kernel。公共FrameLimits/Record/Receipt仅表示字节，不授予删除权；原213字节Header和112字节帧开销保持，JournalRecord/selection/events/CleanupWitness仍在应用层，无store反向依赖。
- 原open_owned_regular创建正文和旧IO/Flock/frame副本已移除。旧`pakro-store/src/store.rs`、backend/util锁和生产CLI仍有后续消费者，不在本轮整仓替换；LanguageLease/实际内容发布留R06，联合提交R15，显式prune R21。

## R05 验证与审查

- `target/r05-store-final.log`：52项全部通过，4.37s。
- `target/r05-app-final.log`：prefix203项中201通过、2旧fixture前置失败，114.98s；`target/r05-app-remaining.log`两项迁移后的精确复测+catalog13+journal14通过，30.64s。生产源码未再改，合计本机282个不同用例均有通过证据，其中63新增、219原调用链。
- `target/r05-final-check.log`：定向格式、diff和非测试`cargo check -p pakro --lib --all-features`通过，70.40s。所有组均未超时；空进程helper外层过滤，真实父测试驱动子进程。
- Spec与Standards分别独立审查，0剩余阻塞。既有warning、链接器和future提示保留。Linux专用非UTF8、真实只读挂载、实际共享安装、CLI和跨平台矩阵未测，不冒称完整产品验收。
- 完整RED/编译前置/fixture失败分类、时间校正及GREEN在R05日志；不要改写历史失败输出，不把新通过重复累计。

## R04 已完成的接续接口

- `libraries/language/identity.rs`：TargetIdentity显式OS/arch/libc/ABI/兼容条件，LanguageKey含实现、精确发行版本、target、Artifact或BuildRequest输入、recipe。当前只接受CPython/PyPy版本语义，不实现Java/Node生态。键不含ProjectId/本机路径，不证明真实TargetFacts或实际对象。
- `libraries/package/identity.rs`：ArtifactId保留SHA256/SHA512原字节摘要；OutputDigest独立于BuildRequestId；PackageInstanceId::python含规范名/版本、SourceIdentity、role、target及依赖上下文；BuildRequestId固定源码/host/target/构建依赖/工具链/配置/配方；SharedLayoutKey含language/graph/build requests/target/recipe，实际输出摘要不反向参与它。
- IdentityHash统一domain+版本1+长度framing，字段/累计文本/原始集合先限额再规范化；独立Python黄金值已测试。SourceIdentity是逻辑来源指纹描述，不是认证许可。
- 原 `python_environment/model.rs` 仍是唯一状态权威。GenerationRecord去owner，显式target + nullable shared_layout。new是generic事实描述，new_shared显式携带完整共享键；register_shared_generation同键同事实返回canonical allocation，冲突拒绝；普通register及codec/state均不能绕过唯一性。shared_layouts只是派生索引，不进wire、不成第二权威。
- SelectionKey/Stamp、CAS、revision、tombstone、事务nonce保留。applied_stamp由事务历史派生，旧nonce重放返回原stamp且不回卷后续选择。typed引用新增Language/Package/BuildRequest/BuildOutput；键类仅SHA256，原始Artifact和实际BuildOutput保留算法；shared记录去重后恰好一Language并拒绝旧Runtime/BuildInput冒充新key。
- catalog codec在原bounded seeds与streaming writer上升为schema2；target/shared_layout必须出现，null仅表示generic。schema1、缺字段、未知字段/版本严格拒绝，不迁移、不当空状态、不删除旧文件。旧内部generic记录仍可表达已有事实，新门面snapshot遇到generic选择明确报错，不自动升级。
- `libraries/project/selections.rs`：ProjectSelections只有CatalogModel与限制。register_content核对target与原始引用预算并用相同model登记；apply_selection首次变更核对完整记录，历史nonce按原model精确重放；ProjectSelection保存key/stamp/layout/语言/包描述。空包集合的language-only布局仍有generation与语言引用；真正clear是None。
- protecting_references保留尚未安全forget的retired记录输入；这是既有内部恢复/清理保护，不能当R21显式prune的永久根。forget_retired仅纯元数据操作，不是实际删除许可。新类型与记录均不授予目录/租约/发布/删除能力。
- journal仅同步target与typed输入检查（read/encode/repository prepare），保留原nonce/expected/allocation归属核验；旧实际ownership保护逻辑不删。联合提交仍留R15，真实共享语言/包分别留R06/R08等。

## 既有 R01–R03 接缝

R01唯一OperationContext/请求/结果合同保持；R02单文件owner保持。R03完整观察与拟议编辑保持，调用方必须先冻结scope/目标项目/发现规则，再observe；恢复改写输入后旧owner撤销并重新观察。旧Config/Tracker/remote_include还有生产CLI消费者，未删除。R03细节见前一交接对应的R03日志及PLAN完成边界，不把只读底层能力当CLI已切线。

## R04 历史验证与审查

- `target/r04-green.log`：44项专项通过，76.33s。
- `target/r04-regression.log`：selections11/model37/catalog13/journal14/cleanup12/recovery26，共113项通过，32.24s；包含23项R04及90项原逻辑测试。后加generic覆盖算入目前45项R04，不把重复运行累计为不同测试。
- `target/r04-final-check.log`：定向格式与diff检查退出0，0.09s。全部组未超时、最终行为0失败/0忽略；直接空环境crash_child夹具被过滤，真实父测试照常驱动子进程。
- Spec/Standards最终复核0阻塞。lib-test38条警告、链接器/future提示保留。未执行实际共享安装、生产CLI或跨平台矩阵，不宣称整项目完成。

## 持续规则

1. 用户最新要求停在已完成的R06，先不启动R07；只有用户新的实施指令才能继续。架构由用户决定，已获授权任务不重复询问开工，不自动Git提交。
2. 后续编译、测试和检查由用户手动执行，Agent读取真实日志确认，不自行续跑。先前20分钟监督约束只在用户重新授予执行权限时适用，不构成当前许可。
3. 不覆盖开工前未提交/未跟踪源码，不恢复用户已清空的旧docs，不删除用户数据。
4. D01取消install-into；D02旧测试数据清理而不迁移，R39再识别路径；D03外部运行/编辑器后置；D04辅助入口保留；D05 Python→Java→Node；D06不自动附带默认包，显式prune才清无项目引用内容，内部在途/恢复保护保留。

## 恢复顺序

本HANDOFF → [逐文件细化记录](log/2026-10-18-9-首版逐文件计划细化.md) → 当前PLAN的F1–F4文件/接口/边界与主架构 → 按需查[D09/D10](log/2026-10-18-8-首版查询删除与精确版本.md)、[快速计划审查](log/2026-10-18-7-快速交付计划审查.md)、[D08](log/2026-10-18-6-取消独立离线模式.md)/[D07](log/2026-10-18-5-项目目录驱动使用方式纠偏.md)、[旧详案](archive/PLAN-R00-R45-2026-10-18.md)和[R06证据](log/2026-10-18-4-R06共享语言库.md)。不要自动实施R07或F1–F4，当前仅更新文档。
