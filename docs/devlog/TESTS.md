# 测试执行规则与用例合同

**当前状态（2026-10-25）：P1 本机 macOS arm64 产品验收通过，自动多进程运行器禁用。** 用户已核对原始手动验收日志：主程序构建成功、扩展准入无提前副作用、which/env/hook同名优先级一致，整组exit0；此前精确安装与资源前后对照继续有效。通用多进程监督器不追加为P1产品门槛；长测试继续由用户终端执行。跨平台、完整ISAT业务及后续阶段边界保持。本次仅同步文档并提交既有实现，不改代码，不重跑已通过且未变化的长测试。

## 2026-10-25再次定向复核

最新手动日志已通过：[all-002758](../../target/p1-recheck-manual-all-20261025-002758.log)。构建1分07秒；扩展准入7秒（11拒绝场景＋NonPy四版本正例）、同名优先级231秒；最终all exit0。不存在本次编译或测试失败，摘要见[结构化核对](../../target/p1-recheck-manual-result-summary.json)。此前001500的E0433记录保留，现已修复并实际构建通过。此次为用户手动执行，automatic_tree_supervision=false；用户已确认这不影响P1本机产品验收通过，自动运行器继续禁用。


前次手动日志[p1-recheck-manual-all-20261025-001500.log](../../target/p1-recheck-manual-all-20261025-001500.log)构建exit101，仅E0433：TaskFileFetcher不存在。已改实际TaskFetcher，当时定向格式0.02秒及diff通过、尚未重编；该失败不计通过，后续成功结果以上述002758日志为准。

**P1本机产品验收通过。** 本轮产品补丁已由用户手动构建及两条定向E2E验证通过；通用多进程严格监督未实现，作为运行器禁用边界保留，不是P1产品阻塞。证据输入：p1-recheck-expanded-admission.json、p1-recheck-command-priority.json、p1-recheck-watchdog-race.json。

已做短检查：68个变更Rust文件在明确单进程内核限制下通过rustfmt；Shell/Python语法与diff通过。监督器能力实验见[capability log](../../target/p1-recheck-watchdog-capability.log)：默认tree在spawn前拒绝，coverage=false/survivors=null；显式single-process阻止fork/posix_spawn，父限制不变，超时的唯一进程被终止。不是通用树监督通过，不再复用此前轮询实验的“无后代”结论。

用户已成功执行：`tests/scripts/p1-manual-recheck.sh all`。需要先设置原3.13.16 stripped归档的PAKRO_TEST_PYTHON_ARCHIVE；脚本检查真实TTY，使用同一target/all-features、关闭增量/DEV/TEST debug，串行构建与2条定向E2E。日志写target。该手动入口没有自动全后代超时保证，不能记为Agent严格监督组；本次按实际002758日志记为产品用例通过，不提升为严格监督通过。

## 上一轮P1审查修复与补证（保留历史覆盖）

上一轮产品路径及补证有实际通过记录；其中监督器轮询方案后来被快速脱离复现推翻，已撤回完整性结论并改为默认拒绝。用户现已确认P1本机产品验收通过；下方上一轮日志保留为真实历史覆盖，后续阶段不自动启动。

| 问题/缺口 | 修复与本次证据 |
| --- | --- |
| 非Pythoninstall/use附带Python包组 | 将实际尾部包组和写目标纳入早准入；11种拒绝及非Python/共享路径正例。[组合回归](../../target/p1-review-combinations.log) |
| 只读exec-env/script/cache绕过 | cache前与直接source前检查；只读错误向上传播，不成功返回缺字段env；cold/warm cache、默认bin、动态bin、正常exec均测。[组合回归](../../target/p1-review-combinations.log) |
| env追加工具丢Python | 按配置加CLI后的有效工具集分流；保留Python PATH/字节码、追加dummy目录，JSON/extended/dotenv/shell及显式Python覆盖均测。[组合回归](../../target/p1-review-combinations.log) |
| which附带命令被缺Python阻断 | 先核实已安装非Python可执行文件与归属，Python错误不盖过已知companion。[组合回归](../../target/p1-review-combinations.log) |
| alias options变成无声明 | 先剥离options识别Python再拒绝；无参/显式、alias环境覆盖及安装准备的精确版本门一致。[核心回归](../../target/p1-review-core-tests.log)、[最终alias与测量组](../../target/p1-review-final-measure.log) |
| 组长退出后监督器遗留后代 | 不以leader.wait作为整树退出证据；私有session/已观察后代追踪，TERM后继续KILL并复查；含另开session、正常退出遗留、无关进程及监督器TERM。[短时实验](../../target/p1-review-watchdog-selftest-final.log) |
| 显式精确Python安装CLI正例 | 冷空库实际CLI获取固定官方tag/SHA256SUMS/真实原归档，核验manifest、真实解释器/prefix，重复零新增请求且声明/lock不变。48.6秒。[冷安装](../../target/p1-review-exact-install.log) |
| A14同配置前后对照 | 原3d2a6c291生产代码与当前代码使用相同fixture body、版本、平台、profile、端口、图/语言identity、64包448文件75456字节；只增加测试计数/注册，不改原生产代码。[对照JSON](../../target/p1-review-resource-comparison.json)、[原版](../../target/p1-review-resource-original.log)、[当前](../../target/p1-review-final-measure.log) |

新增3条组合E2E通过21.6秒；原5条P1边界E2E本轮复跑通过21.4秒；核心18项（资源另测）通过3.4秒；真实共享wheel三项目导入/console/共享隔离通过82.9秒；原非Python查询4条通过14.4秒。见[真实wheel](../../target/p1-review-real-shared-wheel.log)、[非Python](../../target/p1-review-nonpython.log)。不重复运行未变化的全部历史S/恢复长组，也不把它们记为本轮新通过。

A14对照限定**生产选择/应用内核，不含CLI启动和完整ISAT**。前后query包完整核验128、首次apply（含查询）320、重复apply128；语言核验5/9/5；FD均11→1216→11，来源连接0。与上一轮“只计commit”的192不能直接混比。最终样本的query/首次apply/重复apply耗时分别比原版约+5.8%/+4.5%/+4.2%；这是一组小样本观测，不归因为确定的算法变化，也不宣称提速。完整扫描次数及FD没有增长，必要的当前性校验保留，未降低校验强度或提高预算。

前后测量的binary SHA、共用fixture SHA、原版生产文件审计和测试-only补丁均保存在`target/p1-review-baseline/`及对照JSON。原源码通过git archive从3d2a6c291取得，使用同一target/profile串行构建；没有切换/重置当前工作树。复跑driver：`p1-resource-baseline.py --test-filter p1_resource_same_configuration_entrypoints`（当前版需`--ignored`及固定`PAKRO_P1_BASELINE_PORT`），结果由`p1-compare-resources.py`核对identity/文件量/计数。

新的`p1-watchdog.py`用于所有后续长组；`p1-check.sh --review/--watchdog-test/--exact-install`已增加相应入口。监督器预留清理时间在600秒预算内；短实验中的124是预期测试结果，不是整组超时。历史监督器并未在此前真实长组触发600秒，不能把那些通过日志当作其超时分支正确的证据。

## P1本次证据索引

| 组 | 实际结果 | 完整日志 |
| --- | --- | --- |
| 主程序all-features构建 | exit 0，69.1秒 | [final build](../../target/p1-build-final.log) |
| P1及相邻核心 | 17个P1、3个原目录、2个真实安装门面用例通过，141.2秒 | [core](../../target/p1-lib-20261024-200601.log) |
| CLI参数/启动分类 | 2项通过，74.1秒（含编译） | [CLI unit](../../target/p1-cli-unit-20261024-200954.log) |
| P1空库/写准入/只读初始化/非Python派发/选项 | 5条真实E2E通过，26.6秒 | [P1 CLI](../../target/p1-e2e-empty-20261024-200601.log) |
| 真实PBS＋wheel查询及hook | 1条完整E2E通过，155.7秒；含所有env格式、scope、无获取/无登记、缺项和坏TOML撤回 | [shared CLI](../../target/p1-e2e-shared-20261024-201157.log) |
| 原非Python查询 | where、which、未安装版本诊断、bin-paths四条通过，12.4秒 | [non-Python](../../target/p1-nonpython-regression.log) |
| F1–F4核心回归 | 32＋22＋38＋12＝104项通过；4个ignored辅助用例不计通过；366.5秒 | [F regression](../../target/p1-f-regression-20261024-193116.log) |
| S源码构建内核 | S1 11、S2 9、S3 13，共33项通过，162.2秒 | [source unit](../../target/p1-source-unit.log) |
| 真实纯Python源码项目 | iopath 0.1.10／Hydra 1.3.7／ANTLR 4.9.3实际构建、导入/功能、重复与跨项目在网络不可用时复用通过，148.7秒 | [source CLI](../../target/p1-source-real-cli-canonical.log) |
| 限定字节码恢复 | owned隔离库通过，86.3秒；恰好4项恢复，2240项与引用不变 | [recovery CLI](../../target/p1-bytecode-recovery.log) |
| F1命令与F4指定删除 | Ready shim/console、缺失不补装、指定语言/包删除与引用保护、只读查询3条E2E通过，185.9秒 | [terminal/library](../../target/p1-terminal-library.log) |
| 资源/FD | 两次独立进程测量、64包低soft子进程、4项进程上限用例通过，37.7秒 | [resource log](../../target/p1-resources-20261024-202902.log)、[最终同binary测量](../../target/p1-resource-final-binary.log)、[JSON](../../target/p1-resource-baseline.json) |
| 最后合同与完整性回归 | 包inventory/namespace 5、语言批次5、固定build current与损坏1、R01公开合同6、恢复分类2，共19项通过，100.9秒 | [final contracts](../../target/p1-final-contracts.log) |
| 最终格式/注册 | 变更Rust格式、脚本语法、diff检查通过1.4秒；18个P1库用例及清单路径核对通过1.1秒 | [format](../../target/p1-format-20261024-204858.log)、[registration](../../target/p1-list-20261024-205014.log) |

同项目并发补充：[真实竞争提交](../../target/p1-same-project-concurrency.log)1项通过，74.9秒（含测试编译）；两个请求在相同revision准备完成后同时提交，恰好一个成功，另一个stale拒绝，另一项目引用不变。该用例已加入P1精确清单。

以上组存在重叠用例，不把所有行简单相加成“不同测试数”。helpers仅在真实E2E明确调用时证明夹具建立，普通ignored不冒充行为通过。编译仍输出dead_code、链接器和依赖future-incompat警告，不宣称零警告。

## P1 A01–A15对应

| ID | 本次验收依据 |
| --- | --- |
| A01 | shared CLI真实PBS/wheel/console；source CLI真实legacy与源码构建；source unit保留动态build requires、同solver和双证据；配置及语言manifest未改。 |
| A02 | F3 facade与共享布局回归；真实源码同项目/跨项目在网络不可用时复用；不同图及失败保持由F3核心覆盖。 |
| A03 | where语言payload、which/python与console、bin-paths、env各格式、current与project status；F4固定build的current基础版本兼容由terminal/library CLI覆盖。 |
| A04 | 显式X覆盖cwd Y、无项目目标、无声明；profile/cd/env override/no-config/ignore/global/system、别名及本地前缀；glob字符和跨文件alias由P1核心覆盖。 |
| A05 | 缺语言/缺包/缺命令真实CLI；损坏库/旧Python哨兵；无效pin、版本/build歧义、损坏payload/catalog、过期/取消由核心实际文件用例覆盖。不是每个错误分支都另有CLI注入。 |
| A06 | 启用auto-update/lazy/cache prune/purgatory设置与计数HTTP；完整data/state/cache/config/project快照不变，真实TTY提示无持久写；Ready环境启用有效env-cache key仍不写state。 |
| A07 | 显式dummy与损坏Python库隔离、混合/空库/特殊flags/JSON/补全；原四条非Python查询与真实shim/wrapper/lazy/not-found通过；未支持的Python options明确拒绝。 |
| A08 | 真实hook进入→缺失→离开→坏TOML撤PATH/自有字节码差异且引用保留；观察后改写、already-applied取消/改写、Config旧输入与新观察桥接由P1文件receipt用例覆盖。 |
| A09 | use/unuse/upgrade/deps add/remove/旧deps install/prune、混合/别名/依赖补装/force以及任意install-into从真实CLI早拒绝；HTTP和目录快照证明没有先产生副作用。 |
| A10 | 真实Backend force/postinstall回调计数为0；真实Toolset未知metadata在插件建立前失败；原rollback/system/purge/指定删除保护通过F回归。 |
| A11 | 原恢复分类与真实恢复E2E：dry-run/quiet/坏参数/精确4项边界，未操作用户真实实例。 |
| A12 | 同项目真实双线程CAS竞争、F1旧引用保持、F3并发发布/同图共享、F4新引用与指定删除交错、活lease与未知状态；项目→对象→catalog顺序保留。 |
| A13 | 真实64包布局low-soft256/hard4096子进程、hard/父进程不变、高soft不降低与释放；完整manifest/namespace保护由F及最后s4回归覆盖。 |
| A14 | 固定64-wheel查询/apply双进程JSON，记录观察、匹配＋核验、提交重获、计数、FD、文件/字节及来源连接；见下方范围。 |
| A15 | 当次非零注册、真实F/S/CLI、非测试构建和最后定向合同/格式结果；完整ISAT、其他平台未覆盖范围保持。 |

## 资源基线的实际含义

固定64个真实wheel树，448个包文件、76608字节；runtime是确定性仓库fixture，**不是完整ISAT或真实标准库大图的性能数据**。冷进程与重复进程各做首次query/apply和已关联重复query/apply：匹配＋完整核验约413–417ms，首次提交重获约560–564ms；观察单独记录。每次query完整包inventory为128，首次commit为192，已匹配关联commit为0。两进程来源连接均0（内部offline=false），FD分别11→1227→11与11→1228→11，峰值为1ms采样。

旧binary的独立64包容量夹具为31→2283→31（[旧FD记录](../../target/p1-old-fd-metrics.log)）；本次同一容量夹具通过hard4096和释放断言。它与上述query/apply夹具持有阶段不同，不能据两个峰值宣称复杂度或速度改进；也不把历史ISAT 32.71秒与此fixture直接比较。未提高300秒查询边界或4096软FD目标，未加入跨操作hash缓存或削弱核验。

## 可复跑入口与边界

`tests/scripts/p1-check.sh`支持`--list/--build/--lib/--cli-unit/--e2e-empty/--e2e-shared/--f-regression/--source/--resources/--format`，每次调用是独立600秒组。精确注册见`p1-test-cases.txt`；日志写入target，失败根仅保留本轮owned目录供定位。完整源码/历史CLI可按上表原入口复跑，不将全部历史测试自动加入每一组。

真实shared E2E需要显式`PAKRO_TEST_PYTHON_ARCHIVE`指向已验证的CPython 3.13.16+20261013 arm64 macOS stripped归档；恢复及原S实际源码driver使用显式`PAKRO_BYTECODE_RECOVERY_ARCHIVE`的3.11.17原归档。P1资源driver从Cargo输出取得test executable，没有硬编码本机binary hash或ISAT路径。原S历史driver的本机代理/路径限制仍记录为后续验收设施工作。

完整ISAT自动冷组、24根完整图复用/全业务/NMS/模型推理、其他平台/架构/PyPy和外部进程完整生命周期仍未由P1覆盖；原ISAT人工启动证据保持原含义。P2锁恢复、P3声明事务、P4批量回收等未实施。动态执行配置、未知依赖metadata或不支持的共享查询组合会明确诊断，不静默执行后再拒绝。

前期编译/夹具/runner失败和修复保留在[本轮日志](log/2026-10-24-6-P1实施.md)，没有将失败或零测试回写成通过。每组均未超过600秒；最长已结束组为380.9秒（runner失败组），最长上述通过组366.5秒。历史规则和记录如下，dev-flow流程不恢复。

## 历史基线与当时待新增项（2026-10-18）

- 当前F1–F4的逐文件测试合同与未来手动入口统一见[PLAN](PLAN.md)。`tests/scripts/f-manual-check.sh`、`f1-test-cases.txt`至`f4-test-cases.txt`及新F测试现在尚不存在，不能执行空过滤器后记通过；实施时逐阶段创建/注册，验证仍由用户手动运行。下方历史基线和R任务合同不是首版每次必须重跑的队列。
- B0/B1/B2/B3的包、测试源码和runner当前存在；已确认用户的 B0 release、三个核心库测试编译、CLI test binary 编译和 B1 三项行为用例通过；其余分项依实际反馈记录，不能从源码存在或列举成功推断通过。
- R01 的30项模块测试（其中请求16项）、6项外部接口测试和8项原解析回归已通过；其他未来任务测试、新共享 E2E 和 render-cli example 仍需按任务创建注册，不能用空过滤器求绿。
- R02 原六项及新增五项本机测试、文件观测8项、包解析8项与定向格式检查已通过。Linux 专用非 UTF-8 实际文件/alias 用例未在当前 macOS 执行，不能计入通过；原 parser 的信任交互/日志策略未改变。
- R03 的58项本机行为用例及65项原核定向回归已通过；配置发现/静态解析14、项目快照17、拟议编辑11、namespace5、context11。生产CLI、安装、事务和跨平台不在该结论内。
- R04目前45项专项均有实际通过证据：身份22、selections11、model/codec10、journal2。另有90项原逻辑回归通过；113项回归运行总数包含其中23项R04用例，不重复计数。
- R05本机63项新增与219项原调用链回归共282项均有通过证据；两项旧fixture前置失败已迁移并精确复测，非测试库构建、定向格式及diff通过。Linux非UTF8与真实只读挂载未测。
- R06用户手动日志合并确认150项不同测试通过（专项93、相邻回归57），含本机完整官方CPython共享两项；三包非测试check、37文件格式检查与diff检查通过。完整CLI及Linux/PyPy真实发行矩阵不在本次验收范围。
- D07目录驱动使用方式已由用户确认，但目前仅修订文档，没有实现或新增可运行测试；后续必须验收已有即用、缺项未经选择零下载、用户选择安装后可用、A/B包可见性独立、必要适配无新增操作步骤。不用R06通过冒充D07已经接通。
- 实际平台/解释器测试不能用旧/private/tmp路径或假JSON替代。输入由用户明确指定，缺条件记未执行。
- CLI测试在src/main.rs的binary中，用--bin pakro；program/library测试注册到src/lib.rs后用--lib。

## B0 当前构建基线

**现在要测试什么：** 看看当前代码能否编译，debug和release是否已有问题。还没改重构代码，失败先记录为基线。

**测试命令：**

~~~bash
cd '/Users/fuxinao/文件/project/pakro'
export CARGO_TARGET_DIR="$PWD/target"
bash -euo pipefail <<'SH'
cargo build -p pakro --bin pakro --all-features
cargo build -p pakro --bin pakro --all-features --release
cargo test -p pakro-index -p pakro-resolver -p pakro-store --lib --all-features --no-run
SH
~~~

逐条运行。Rust最低版本以Cargo.toml为准；不自动安装工具链或升级依赖。用户已于 2026-10-17 反馈 reshim 模块条件与使用点不一致的 E0432/E0433 编译失败；单行修复后 release 复测通过，不需重复该项。核心库测试编译及 B1 继续由用户执行，实际证据见末尾。

## B1 当前精确CLI回归

**现在要测试什么：** 看看远端查询失败会不会被当成空列表，JSON有没有预发布标记，以及真的有新版本才报告能升级。

**测试命令：**

~~~bash
cd '/Users/fuxinao/文件/project/pakro'
export CARGO_TARGET_DIR="$PWD/target"
bash -euo pipefail <<'SH'
for test_name in \
  'cli::ls_remote::tests::test_single_tool_listing_surfaces_recorded_backend_failure' \
  'cli::ls_remote::tests::test_all_json_output_always_carries_prerelease' \
  'cli::outdated::python_package_tests::only_actual_available_candidate_is_marked_outdated'
do
  listing="$(cargo test -p pakro --bin pakro --all-features "$test_name" -- --exact --list)"
  printf '%s\n' "$listing"
  printf '%s\n' "$listing" | grep -Fx "$test_name: test" >/dev/null
  cargo test -p pakro --bin pakro --all-features "$test_name" -- --exact --nocapture
done
SH
~~~

每条实际执行一个测试。不能改成--lib，也不能以退出码0但0个测试代替通过。

本组名称匹配使用系统已有的 `grep -Fx`，不依赖 `rg`。用户运行环境缺少 `rg` 已造成一次名称检查中断；已编译的 B0 无须重跑，只继续 B1，并保存到独立的 `target/r00-cli-tests.log`，保留此前失败日志。

## B2 当前核心库与真实文件回归

**现在要测试什么：** 看看来源解析、求解、文件身份和锁保护的基线。native配置接口已在 R02 实现并定向通过；其他范围仍以实际执行证据为准。

**测试命令：**

~~~bash
cd '/Users/fuxinao/文件/project/pakro'
export CARGO_TARGET_DIR="$PWD/target"
bash -euo pipefail <<'SH'
cargo test -p pakro-index --lib --all-features
cargo test -p pakro-resolver --lib --all-features
cargo test -p pakro-store --lib --all-features
cargo test -p pakro --lib --all-features 'config::config_file::native_python::tests::' -- --list
cargo test -p pakro --lib --all-features 'config::config_file::native_python::tests::' -- --nocapture
cargo test -p pakro --lib --all-features 'lockfile::transaction::' -- --nocapture
SH
~~~

不必每步重跑全套；优先直接相关范围，有新变化或风险才扩大。原配置owner六项测试保留断言。

## B3 当前E2E入口

**现在要测试什么：** 看看现有CLI能否通过独立脚本测试，不依赖以后将删除的产品run命令。

**测试命令：**

~~~bash
cd '/Users/fuxinao/文件/project/pakro'
export CARGO_TARGET_DIR="$PWD/target"
bash -euo pipefail <<'SH'
cargo build -p pakro --bin pakro --all-features
test -x "$CARGO_TARGET_DIR/debug/pakro"
bash tests/e2e/run_test cli/test_registry
SH
~~~

- 当前案例有ASDF等映射期望，R43裁剪后按新范围迁移，不能永久保留已退出来源。
- runner需要bash、jq及GNU兼容mktemp/realpath等环境，本次没有验证本机前提。
- runner通过CARGO_TARGET_DIR/debug选择二进制；不要仅用PAKRO_E2E_BIN保证native测试目标。
- runner创建隔离HOME/DATA/CACHE/STATE，不把真实用户目录作为测试数据。
- 显式传测试文件。不传tests/tasks/test/e2e参数可能执行零项；run_all可能跳慢测或受tranche/release-skip影响，不能直接叫全量。

## T0 R00 运行器测试先行

现有文件 `tests/scripts/test-e2e-runner.py` 已扩充为 14 个测试方法，保留原 10 项，新增 4 项 R00 用例（空选择用例包含 3 个子场景）。该文件有现成 unittest 入口，复制真实运行器到临时目录，用 stub executor 观察实际调度，不安装语言、不执行 Pakro 产品功能。

**现在要测试什么：** 看看空清单现在能否给出明确说明，跳过的测试是否都列明名称、原因和数量，同时确认原来的并发、重试和显式列表功能仍正常。用户已提供红灯；本次复测同一组 14 项，应全部通过才可记录完成。

**测试命令：**

~~~bash
bash -euo pipefail <<'SH'
cd '/Users/fuxinao/文件/project/pakro'
mkdir -p target
python3 -B tests/scripts/test-e2e-runner.py -v \
  2>&1 | tee target/r00-runner-green.log
SH
~~~

用户已实际运行 14 项并全部 OK（8.265s），当前无需重复运行。red 日志为 14 项/6 处失败，green 为同一组 14 项通过；原有 10 项与新增断言保持。本组只证明 runner 调度/报告，不替代真实产品 E2E。

## T1 R01 完整合同验收

R01 已完成：库内 30 项（请求16、上下文7、报告4、私有位置描述3）、外部独立 crate 6 项及原 parser 8 项全部通过。最后一次受影响验证及格式检查见 target/r01-reviewed-green.log（145.43s，未超时）；原 parser 8 项见 target/r01-final-green.log，之后该文件未变。

已完成的 RED→GREEN 包括未实现入口、策略边界、上下文/报告/位置、归一化真实取消与公开构造。未改变行为的新增覆盖不伪造 RED。每组 Agent 命令共同上限为 1200 秒，超时停止并交用户。没有新变化时不重复已通过检查。

**现在要测试什么：** 五种安装意图不混淆，原声明及归属保留，策略/取消/期限生效；缺来源或目标不猜，路径描述不创建目录；失败和取消保留已经发生的提交状态，独立 CLI crate 可以使用有限公开接口。

**必要时的完整复测命令：**

~~~bash
bash -euo pipefail <<'SH'
cd '/Users/fuxinao/文件/project/pakro'
export CARGO_TARGET_DIR="$PWD/target"
mkdir -p "$CARGO_TARGET_DIR"
exec > >(tee "$CARGO_TARGET_DIR/r01-contract-verify.log") 2>&1
listing="$(cargo test -p pakro --lib --all-features r01_ -- --list)"
printf '%s\n' "$listing"
count="$(printf '%s\n' "$listing" | grep -c ': test$')"
test "$count" -eq 30
cargo test -p pakro --lib --all-features r01_ -- --nocapture
listing="$(cargo test -p pakro --test r01_program_contract --all-features -- --list)"
printf '%s\n' "$listing"
count="$(printf '%s\n' "$listing" | grep -c ': test$')"
test "$count" -eq 6
cargo test -p pakro --test r01_program_contract --all-features -- --nocapture
SH
~~~

数量检查不替代查看实际结果：0 tests、ignored 或未执行项不算通过。这里证明 R01 基础合同；真实配置观测、规范库根/身份、语言共享/租约、来源会话、事务及生产 CLI 接线仍由后续任务验收，不据此宣布完整安装功能或跨平台通过。

## T2 R02 真实配置观测验收

本机 macOS：11项配置观测（保留原6项）、8项文件观测回归、8项包解析回归全部通过，无失败或忽略；定向格式检查通过。日志 target/r02-native-config-green.log（30.58s）与 target/r02-regression-green.log（1.88s），各组均未超时。Linux 专用真实非 UTF-8 文件/别名项尚未执行，不计入通过。

**现在要测试什么：** 文件被替换、原地修改、别名换掉或期限已过时，旧 owner 能否及时拒绝读取和发出凭据；坏依赖声明是否明确报错，返回的请求副本是否与 owner 独立。完整配置用例复测命令见 [PLAN R02](PLAN.md#r02补齐现有-native-配置观测桩)，逐项核对完整名称，使用系统 grep，不依赖用户安装 rg。

**必要时的相关回归命令：**

~~~bash
cd '/Users/fuxinao/文件/project/pakro'
export CARGO_TARGET_DIR="$PWD/target"
bash -euo pipefail <<'SH'
for filter in 'lockfile::transaction::observation::tests::' 'packages::request::tests::'; do
  listing="$(cargo test -p pakro --lib --all-features "$filter" -- --list)"
  printf '%s\n' "$listing"
  count="$(printf '%s\n' "$listing" | grep -c ': test$')"
  test "$count" -eq 8
  cargo test -p pakro --lib --all-features "$filter" -- --nocapture
done
rustfmt --check --edition 2024 --config skip_children=true \
  src/core/config/config_file/native_python.rs \
  src/core/config/config_file/native_python/tests.rs \
  src/core/config/config_file/pakro_toml.rs
SH
~~~

返回错误脱敏只覆盖错误值；原 PakroToml parser 的信任交互/登记与日志行为不变。此处证明源不写/无 lock guard，不替代后续查询 ReadPolicy、完整项目优先级或真实安装验收。

## T5 R05 受控 IO 与实际锁

**已验证：** 查未初始化库不创建；已初始化查询使用只读FD；缺失/替换稳定锁及目录、跨库/错key锁拒绝；项目→排序对象→catalog和原deadline贯穿，独立子进程真实SH阻挡EX。CAS/有界帧、原ownership与故障恢复、prefix写入和清理保护保留。

**实际结果：** `target/r05-store-final.log` 52项通过（4.37s）；`target/r05-app-final.log` prefix203项中201通过、2旧fixture前置失败（114.98s）；`target/r05-app-remaining.log` 修复夹具后的2项精确复测及catalog13/journal14全部通过（30.64s）。这是282个不同用例，63新增、219原调用链，不累计子进程helper或重复执行。`target/r05-final-check.log` 格式、diff、非测试库cargo check通过（70.40s）。所有组未超时，既有编译/链接/future警告保留。

下面是完整定向复验范围；已过且未变的组不需重复。Agent执行须再套共同1200秒监督并保存日志，超时不得分拆续跑。Linux专用非UTF8用例未在macOS执行；fcntl实查O_RDONLY已通过，但真实只读挂载未测。生产CLI和实际共享安装不属于本组结论。

```bash
cd '/Users/fuxinao/文件/project/pakro'
export CARGO_TARGET_DIR="$PWD/target"
bash -euo pipefail <<'SH'
listing="$(cargo test -p pakro-store --lib --all-features 'control::tests::r05_' -- --list)"
printf '%s\n' "$listing"
for name in r05_read_empty_root_creates_nothing r05_shared_reader_blocks_collector r05_lock_inode_replacement_is_rejected; do
  printf '%s\n' "$listing" | grep -Fx "control::tests::$name: test" >/dev/null
done
cargo test -p pakro-store --lib --all-features -- --nocapture --skip control::lease::tests::r05_process_lease_helper
for filter in 'packages::python_environment::prefix::' 'packages::python_environment::catalog::tests::' 'packages::python_environment::journal::tests::'; do
  listing="$(cargo test -p pakro --lib --all-features "$filter" -- --list)"
  printf '%s\n' "$listing" | grep -F "$filter"
  cargo test -p pakro --lib --all-features "$filter" -- --nocapture --skip packages::python_environment::prefix::control::lease::tests::lease_process_helper --skip packages::python_environment::prefix::observer::recovery::tests::crash_child
done
SH
```

外层跳过无环境会直接返回的helper，父测试仍设置真实环境启动它们；不能将helper空执行算通过。具体每轮RED、编译/fixture前置失败与GREEN分类见[R05日志](log/2026-10-18-3-R05受控IO与锁顺序.md)。

## TDD与失败判定

1. 在任务规定文件写真实前置、操作和断言，注册mod。
2. Agent 按时限或用户运行取得失败；编译错误、夹具失败不是目标行为红灯。
3. 在授权范围改最小实现，复用原函数，不弱化断言。
4. Agent 按时限或用户复测同一组，检查实际用例数和结果。
5. 新分支首次即通过只记覆盖，不伪造它曾失败。
6. helper通过但真实CLI未调用，仍是未完成接线。
7. 小范围由 Agent 按 20 分钟时限运行；长任务及超时后由用户执行，实际结果到达前保持待测。

## 各类测试要证明的事实

| 类型 | 真实输入/操作 | 必须断言 |
| --- | --- | --- |
| 共享语言 | A首装、B重复请求、重启再打开 | 唯一实际payload；解释器/stdlib一致；无重复获取 |
| 共享包 | 相同图和部分不同图 | 共同内容复用、不同依赖不串、写入不污染共享对象 |
| 配置 | 替换inode、别名或新增覆盖文件 | 提交拒绝，用户字节保留，不混旧Config与新receipt |
| 事务 | 每个文档/catalog窗口注入故障 | 未提交只恢复自己；已提交只收尾；不覆盖他人 |
| 显式 prune | 手工提前安装、项目直接/传递引用、内部在途操作、孤立对象组 | 未调用时不全库清理；调用时无引用内容可删；历史记录不永久保活，内部使用与未知状态保护 |
| 缓存清理 | 新旧缓存、正式语言/包、在途暂存 | cache clear/prune 只清可丢弃缓存；不卸载正式库、不破坏在途工作 |
| 默认包退出 | 隔离旧默认包文件后安装语言 | 不自动安装清单包、不生成项目依赖；用户明确选择及必要传递依赖仍正常 |
| 外部运行保护（后置） | VSCode/直接启动/子进程 | R20 后续单独验收，不计入当前完成门 |
| 查询 | 空库、缺项目、坏来源、并发切换 | 无安装/登记/修复；缺失、损坏、无候选分开 |
| 来源/锁 | HTTP夹具、枚举/认证回调计数 | Locked不重选；Offline零网络；摘要/metadata一致 |
| 构建 | 实际worker/hook/编译器、联网/超时尝试 | 阻止未声明获取，失败不换选择，prefix/receipt正确 |
| 平台 | 实际CPython/PyPy、Linux/macOS、扩展 | 不猜目标，不用shell成功JSON冒充解释器 |

纯算法、真实文件、多进程、真实CLI、实际解释器分别记录，不能用一个mock代表全部。

## 完整测试示例一：保留现有配置owner用例

该用例已存在于src/core/config/config_file/native_python/tests.rs，R02直接保留，不重复新增。RAW_ONE/load/version及导入来自同一个现有文件。

~~~rust
#[test]
fn actual_file_uses_the_existing_parser_and_checked_requests_without_writing_a_guard() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pakro.toml");
    fs::write(&path, RAW_ONE).unwrap();
    PakroToml::from_str(RAW_ONE, &path)
        .expect("existing parser accepts the real fixture; this is only a data prelude");
    let before = fs::symlink_metadata(&path).unwrap();
    let owner = load(&path);
    assert_eq!(version(&owner), "1.0");
    owner.for_python().unwrap().recheck_for(&owner).unwrap();
    let after = fs::symlink_metadata(&path).unwrap();
    assert_eq!(
        (before.dev(), before.ino(), before.mode(), before.mtime(), before.ctime()),
        (after.dev(), after.ino(), after.mode(), after.mtime(), after.ctime())
    );
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    assert!(!directory.path().join(".pakro.lock.guard").exists());
}
~~~

其他五项保留在源码中，不在docs复制整套会漂移的实现。

## 完整测试示例二：未来本地查询不写目录

计划文件：tests/e2e/refactor/test_query_empty_library_is_read_only，R26创建，**目前不存在**。使用run_test的隔离目录，只证明目录无变化，不替代HTTP计数测试。

~~~bash
#!/usr/bin/env bash
set -euo pipefail
export PAKRO_OFFLINE=1

snapshot() {
  python3 -B - <<'PY'
import hashlib
import json
import os
import stat
from pathlib import Path

rows = []
for key in ("PAKRO_DATA_DIR", "PAKRO_CONFIG_DIR", "PAKRO_STATE_DIR", "PAKRO_CACHE_DIR"):
    root = Path(os.environ[key])
    if not root.exists():
        rows.append((key, ".", "absent"))
        continue
    entries = [root]
    for parent, directories, files in os.walk(root, followlinks=False):
        entries.extend(Path(parent) / name for name in sorted(directories + files))
    for path in sorted(set(entries), key=lambda value: str(value)):
        info = path.lstat()
        relative = "." if path == root else str(path.relative_to(root))
        if stat.S_ISREG(info.st_mode):
            value = hashlib.sha256(path.read_bytes()).hexdigest()
        elif stat.S_ISLNK(info.st_mode):
            value = os.readlink(path)
        else:
            value = ""
        rows.append((key, relative, info.st_mode, info.st_size,
                     info.st_mtime_ns, info.st_ctime_ns, value))
print(json.dumps(rows, ensure_ascii=False, sort_keys=True))
PY
}
before="$(snapshot)"
pakro project ls >/dev/null
after="$(snapshot)"
if [[ "$before" != "$after" ]]; then
  printf '%s\n' '查询修改了隔离的业务或缓存目录。' >&2
  exit 1
fi
~~~

本地project ls不需要刷新远端缓存。其他查询允许metadata缓存写入时另测，不能拿该例冒充所有缓存政策。

## 完整测试示例三：核心命令没有被合并

计划文件：tests/e2e/refactor/test_core_help_keeps_distinct_commands。R41准备，R43裁剪后使用。**不是现在就应通过的测试。**

~~~bash
#!/usr/bin/env bash
set -euo pipefail
export NO_COLOR=1
export CLICOLOR=0

while read -r first second; do
  if [[ -z "$second" ]]; then
    pakro "$first" --help >/dev/null
  else
    pakro "$first" "$second" --help >/dev/null
  fi
done <<'COMMANDS'
install
use
upgrade
deps install
deps add
uninstall
unuse
prune
deps remove
ls
ls-remote
search
registry
latest
outdated
current
tool
where
which
bin-paths
project ls
project status
installs ls
installs select
cache clear
cache prune
cache path
doctor
doctor path
doctor project
test-tool
env
version
self-update
implode
COMMANDS

for removed in bootstrap dotfiles daemons oci mcp install-into; do
  if pakro "$removed" --help >/dev/null 2>&1; then
    printf '已排除命令仍可分派：%s\n' "$removed" >&2
    exit 1
  fi
done

if pakro --help | grep -E '^[[:space:]]*find([[:space:]]|$)' >/dev/null; then
  printf '%s\n' '不能以新find入口替代现有查询命令。' >&2
  exit 1
fi
~~~

install-into 已按 D01 加入明确退出断言；D04 辅助命令增加保留断言。旧 installs migrate 本轮不扩建、也未授权删除，不列作新管线完成要求。本例只测帮助路由，不执行更新/卸载；真实行为、权限和输出由 R43 等对应测试另验，不能据 help 成功宣布功能已完成。

## 真实解释器输入

R00/R13 给当前共享安装/构建场景增加明确测试输入；R20 外部运行接入后置，届时复用这些输入，例如PAKRO_TEST_PYTHON_ROOT和PAKRO_TEST_PYTHON_EXE。它们不是产品配置；当前runner尚未透传，必须先实现。

- 用户提供现有解释器，不猜路径、不自动下载、不复用旧/tmp地址。
- 原安装只读，不chmod、删除或修补用户解释器求绿。
- 实际入口、stdlib、目录身份和内容参与断言，不能只比较返回字符串。
- 缺平台、解释器或隔离条件标未执行/失败原因，不包装通过。
- Agent在接口真实存在后给完整命令，不编造用户机器上的位置。

## R06 手动交接（2026-10-18）

**R06验收已完成，无需继续运行命令。** 用户手动的`target/r06-manual-20261018-180052.log`与`182044.log`合并去重150项行为测试通过（R06专项93、相邻回归57），三包非测试cargo check通过；最终`182740.log`有--format-only完整成功标记，37文件格式检查与git diff --check全部通过。Agent仅核对日志，没有代用户运行收尾验证。

用户明确要求先不启动R07，后续编译和测试继续由用户手动执行。下列入口仅保留供未来必要复现，不要求重复已通过的检查。

**已完成的最后格式收尾命令（记录）：**

```bash
cd '/Users/fuxinao/文件/project/pakro'
bash tests/scripts/r06-manual-check.sh --format-only
```

`--format-only`仅对`tests/scripts/r06-format-files.txt`列出的R06文件执行rustfmt排版，然后逐个`--check`和`git diff --check`，收集所有剩余差异后返回结果。这个选项不调用Cargo、不执行测试、不要求真实归档存在，也不清理任何文件。新的日志记录mode和完成提示。

历史续跑入口`--remaining`保留，但当前不需要重复：它覆盖Sigstore4项、util5项、5组相邻回归及编译/格式检查。最近续跑实际全部行为通过：Sigstore2+2、util5、相邻19+20+10+15+8；与主库84项去重共150项，不累计零测试、子进程helper或重复用例。

不传`--remaining`仍是完整手动验收：显式使用已授权的`/Users/fuxinao/文件/project/pakro_test/cpython-3.13.16+20261013-aarch64-apple-darwin-install_only_stripped.tar.gz`、版本3.13.16及已核对SHA256；缺文件即停止，不自动下载。测试在隔离临时库运行，不修改原归档。仅当共享库源码又变化或需要重新验收全部内容时使用完整模式。

完整模式依次执行（`--remaining`只替换第一项，`--format-only`跳过所有编译和测试）：

- pakro、pakro-util、pakro-sigstore的R06专项，使用`--include-ignored`实际运行两项真实归档验收；只跳过父测试会自行启动的两个process helper。
- native plan/extract/旧来源缓存、旧prefix inventory和sysconfig相邻回归。按模块过滤时，没有匹配用例的其他crate会显示0 tests，这不计为该crate通过；实际目标用例必须运行。
- 非测试库`cargo check`、R06文件清单的定向`rustfmt --check`及`git diff --check`。失败立即停止，不继续使用旧可执行文件冒充通过。

为控制磁盘，脚本设`CARGO_INCREMENTAL=0`、`CARGO_PROFILE_DEV_DEBUG=0`、`CARGO_PROFILE_TEST_DEBUG=0`，所有Cargo命令保持同一package/feature组合。不改Cargo.toml，不构建release，不自动清理；沿用用户刚完成的同配置构建产物，不能承诺具体峰值空间。

完整输出自动保存在`target/r06-manual-YYYYMMDD-HHMMSS.log`，内含执行mode。格式模式会实际修改清单内文件的排版，不修改业务逻辑；Agent没有执行该模式。

三份日志的证据已齐全，R06已勾选完成。此前HTTP前置失败和格式差异均已取得最终通过结果；零测试、忽略和重复执行不计入150项。完整生产CLI、Linux/PyPy真实发行和workspace全量验证不属于本次通过声明。

## Q0 最终质量检查

前置：R41工程render-cli example已创建，R43/R44已完成。以下不是当前基线命令。

**现在要测试什么：** 看看整体构建、核心行为、代码检查和生成资源是否一致；debug不代表release，本机不代表全平台。

**测试命令：**

~~~bash
cd '/Users/fuxinao/文件/project/pakro'
export CARGO_TARGET_DIR="$PWD/target"
bash -euo pipefail <<'SH'
cargo fmt --all -- --check
cargo build -p pakro --bin pakro --all-features
cargo build -p pakro --bin pakro --all-features --release
cargo test --workspace --all-features --all-targets
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo run -p pakro --example render-cli -- --check
cargo package -p pakro --list --allow-dirty
SH
~~~

逐条执行并反馈。生成器--check只比较，不改文件；--write只在用户要更新资源时手动执行。编译、格式、Clippy均不替代行为验证。

核心E2E使用R45明确文件列表，真实平台矩阵单列；跳过的slow/ignored/tranche项不能说已通过。生态分阶段严格按Python→Java→Node：先完成M-PY，再验Java，再验Node；Go 和独立 Rust 生态不是本轮验收前置。R20 编辑器/外部运行保护单列后置，不用跳过来冒充通过。

## 用户反馈记录

### 2026-10-17：R00 起始基线

- 源码状态：`main` / `26e8cc024066c535f230677ea3c7a8ef59672563` 加现有全部未提交及未跟踪文件；完整 Git 短状态见 [R00 日志](log/2026-10-17-4-R00开始执行与基线记录.md)。本轮未修改业务源码、Cargo 或测试脚本。
- B0 debug/release 构建与核心库测试编译：**待用户测试**。
- B1 三个精确 CLI 测试：**待用户测试**；测试名称及 binary 注册已从源码核对，未执行列举或用例。
- 平台、Rust 版本、实际用例数、失败/跳过及原始输出：等待用户提供；不从当前终端目录或历史日志推定通过。
- B2/B3、真实解释器和跨平台场景：**未执行**，本次先交付 R00 要求的 B0/B1。
- 静态观察：`reshim` 模块只有 debug 注册，但 `asdf.rs` 无条件导入；release 是否失败仍以 B0 输出为准，尚未修复。

### 2026-10-17 21:10：B0 条件编译失败与最小修复

- 用户回传：E0432，`asdf.rs:10` 无法导入 `crate::cli::reshim`；E0433，`commands/mod.rs:369` 的 `Reshim(reshim::Reshim)` 无法解析。编译器明确指出模块在 `mod.rs:90` 被 `#[cfg(debug_assertions)]` 排除，最后为 `could not compile pakro (bin pakro) due to 2 previous errors`。原输出摘录存入本轮日志。
- 这是构建前置失败，不是新功能的预期行为红灯。输出片段没有完整命令头、平台和数字退出码，不据此记 debug 构建或其他测试通过。按交付脚本遇错即停，后续库测试编译与 B1 未取得运行证据。
- 修复仅移除 reshim 模块的 debug-only 条件；现有命令入口、转发逻辑和行为断言不变。
- 当前状态：release 修复复测 **待用户测试**；核心库测试编译及 B1 **待用户测试**；本轮没有新增或运行测试。用户下一步先执行 B0 中同一条 release 构建命令。
- 复测输出可使用 `tee` 保存到项目 `target/r00-release-build.log`，保留完整警告与错误，不用屏蔽 warning 掩盖问题。

### 2026-10-17 21:15：B0 release 实际通过

- 用户回传且日志尾部一致：`Finished release profile [optimized] target(s) in 56.64s`。执行命令为此前交付的 `cargo build -p pakro --bin pakro --all-features --release`，完整输出保存在 `target/r00-release-build.log`。
- 状态：release 构建 **实际通过**；本次 reshim 条件编译问题已由该构建验证消除。当前日志没有行为测试执行结果，不把它当 B1 通过。
- 警告基线：`pakro` lib 1128 条，包含未使用导入和未接通的函数、方法、类型等 dead_code；另有 `proc-macro-error2 v2.0.1` 未来 Rust 兼容性提示，日志未提供具体诊断。保留事实，不执行 cargo fix、不加 allow、不顺手升级依赖；这不是最终无警告质量门通过。
- B0 debug 构建尚无独立输出记录；核心库测试编译及 B1 三项 **待用户测试**。当前先继续此前失败之后的命令，不重跑 release。
- 下一组命令保存到 `target/r00-baseline-tests.log`；遇失败停止，未执行项目仍待测。列举须精确匹配，每条 B1 实际执行一个测试，零测试不算通过。

### 2026-10-17 21:20：核心库测试编译通过，B1 命令前置失败

- `target/r00-baseline-tests.log`：三个核心库测试编译完成（17.03s）并生成三个 Executable；`--no-run` **实际通过**，没有运行库行为用例。
- CLI test binary 编译完成（1m36s）；首个测试列举为 `1 test, 0 benchmarks`，随后 `bash: line 16: rg: command not found`。三个 B1 行为用例均 **未执行/缺条件**，不是断言失败或行为通过。
- 修正命令：`rg -Fx` 改成系统 `grep -Fx`，保留固定字符串、整行匹配及无匹配时停止；不用 `-q` 提前退出，不安装工具、不修改断言。PLAN 的 121 处及 TESTS 的 1 处名称模板同步；未来帮助示例改用 `grep -E`。
- 警告保留：1128 条库警告、proc-macro-error2 未来兼容性提示，以及 CLI test binary 新增一条 macOS 链接器 `__eh_frame` 超过 16MB 的警告（异常处理性能可能受影响），编译仍完成。不执行 cargo fix，不把警告数当失败用例数。
- 下一步仅运行 B1 三个实际行为测试，保存到 `target/r00-cli-tests.log`；release 和核心库 `--no-run` 不重复。R00 总体、runner 待办和 R01 均未完成。

### 2026-10-17 21:24：B1 实际通过，T0 待用户测试

- 只读 `target/r00-cli-tests.log`，三个明确测试名均有实际执行及 `ok`，各为 `1 passed; 0 failed; 0 ignored; 0 measured; 371 filtered out`。B1 三项 **实际通过**，不将其余 371 项算已执行。
- release 和核心库测试编译已有证据保持；debug 普通构建没有独立输出，仍不与 debug test binary 编译混称。警告仍未清除。
- 新增运行器 4 个测试方法，原 10 项保留，总计 14 项；只完成测试编写，未执行，状态 **待用户测试**。运行器生产脚本未改，尚未获得预期失败证据；不提前记录红灯或绿灯。

### 2026-10-17 21:31：T0 红灯已定位，修复待复测

- `target/r00-runner-red.log`：`Ran 14 tests in 7.248s`，`FAILED (failures=6)`；原 10 项通过，新 4 项产生 6 个失败子场景。
- 空发现、空分片、只有慢测先在真实 run_all_tests 的 FAST/SLOW 空数组展开处触发 unbound variable，未到达预期诊断；属于被测脚本的兼容缺陷，保留断言并修此前置路径。另 3 项为 release-skip 无统计、slow 缺明细、tranche 漏计两项，行为红灯真实成立。
- 修复只改 run_all_tests：兼容空数组、分类保存每项原因、统一报告函数、普通无可执行项报告后失败、显式 release skip 报告后保持旧返回值。全部 14 项测试断言未改，并发/重试实现未改。
- Standards 与 Spec 静态审查未发现阻塞项；状态仍 **待用户测试**，不提前记录绿色结果。下一步执行 T0 现有命令，写入 `target/r00-runner-green.log`。

### 2026-10-17 21:42：补齐 debug 普通构建证据

- 用户答复首轮 debug 普通构建“不记得或没有保留输出”；状态保持 **待用户测试**，不按首轮命令顺序推定通过。
- 仅补交 B0 中普通 debug 构建命令 `cargo build -p pakro --bin pakro --all-features`，完整输出保存到 `target/r00-debug-build.log`；不重跑已有通过证据的 release、核心库测试编译或 B1。
- T0 green 复测继续按上节命令执行，当前尚未收到运行结果。Agent 本次只更新记录，没有运行或修改代码。

### 2026-10-17 21:52：R00 证据收齐

- `target/r00-runner-green.log`：14 个方法逐项 ok，`Ran 14 tests in 8.265s`、`OK`；没有跳过或忽略。R00 runner **实际通过**。
- `target/r00-debug-build.log`：`Finished dev profile [unoptimized + debuginfo] target(s) in 10.46s`；debug 普通构建 **实际通过**。1128 条库警告及一个链接器警告仍在，未来 Rust 兼容性提示仍在，不据此说无警告。
- 合并之前 release、核心库测试编译、B1 三项证据，R00 完成。B2 库行为、B3 真实产品 E2E、真实解释器共享及跨平台场景均未执行，后续按对应任务交付；不扩大测试结论。

### 2026-10-17 22:00：R01 请求红灯测试待用户运行

- 已创建正常模块树与 5 项测试，纯描述类型及 NotImplemented 桩仅支撑测试编译；没有执行编译或测试，不预先记录失败。
- 静态 Standards/Spec 审查未发现阻塞当前 RED 交付的问题。真实 TOML 夹具与描述声明一致；原输入、目标、作用域、策略保留断言；Debug 不显示原始参数或环境值。实际编译仍待用户结果。
- 状态为待用户测试。新接口尚未接 CLI，原 parser/绑定实现未改；R01 的策略校验、上下文/操作报告与必要公开入口仍待后续，不标完成。

### 2026-10-17 22:27：Agent 受限执行的 R01 红绿证据

- 用户将后续小测试交 Agent，明确每组 20 分钟上限。四组实际执行均受共同 1200 秒截止时间监督并保存完整输出，没有超时或自动续跑。
- r01-request-red.log：列举成功，0 passed / 5 failed / 0 ignored，退出 101，94.16s；均为 NotImplemented 的预期行为失败。
- r01-request-green-1.log：R01 5 项与旧 request 8 项全部通过，退出 0，80.4s；定向 rustfmt 同组成功。
- r01-policy-red.log：15 项中 8 passed / 7 failed / 0 ignored，退出 101，75.15s；具体失败为期限、四预算、写域、目标。
- r01-policy-green.log：15 passed / 0 failed / 0 ignored，退出 0，27.29s；当前请求与策略子步骤通过。
- lib-test 仍有 34 条既有/阶段性警告及依赖未来兼容性提示，不将其当无警告构建。现阶段不运行全库/跨平台/真实安装测试。
- 原始输入保留、无 IO 边界、校验顺序与共享纯 helper 已作静态 Spec/Standards 审查。OperationContext/Report、公开 facade 与库门面继续按计划接通，R01 不标整体完成。

### 2026-10-17 23:34：R01 完整验收与审查通过

- r01-contract-red.log：26项，15通过/11失败，81.53s；明确的上下文、报告和位置未实现入口产生真实 RED。
- 修正超出 Spec 的“目录必须完全不嵌套”预期后，red-2 为27项、19通过/8失败（27.1s），报告4项先转绿；contract-green 全部27项通过（29.81s）。
- r01-context-use-red.log：29项，28通过/1失败（67.67s），抓住归一化漏读取消信号；r01-public-red.log 外部6项为2通过/4失败（83.53s），验证公开构造尚未实现。
- r01-final-green.log：库29、外部6、原parser8全部通过，57.12s。
- Standards指出库向程序错误类型反向依赖后，已改为库内错误+程序边界映射，并补充保持外部错误语义的回归。r01-reviewed-green.log 最终库30项、外部6项全部通过，定向格式检查通过，145.43s；原parser8项源未再变化，不重复无关检查。
- 所有组均退出预期状态且未超时，最终测试无失败或忽略。Spec/Standards复核均无阻塞项。既有lib-test 34条、lib 1128条及链接器/future提示继续保留，不以scope外清理凑无警告。
- R01已完成；R02实际配置观测尚未修改。完整验收不包含真实安装、事务恢复或跨平台矩阵。

记录命令、源码状态、平台、解释器、实际用例数、失败/跳过情况、失败属于前置还是目标行为，以及原断言是否保持。

状态仅用：待用户测试、真实预期失败、实际通过、失败待修、未执行/缺条件。不得把期望结果、旧日志或静态推断当实际验证证据。


### 2026-10-18：R03 实际验证与收尾

- 首次RED23项失败（146.70s）；后续一次编译前置错误（41.91s）已修复，不记行为RED。分层RED为44项19通过/25失败（81.01s），随后45项全通过（76.19s）。
- 审查回归RED56项45通过/11失败（33.89s），修正后56项全通过（76.12s）。追加别名与LocalOnly范围回归分别真实失败，再修正。
- 最终 `target/r03-final-green.log`：58 passed / 0 failed / 0 ignored，26.45s、退出0、未超时。覆盖真实文件替换、缺失候选、别名、include、pyproject、锁、冻结输入、scope、声明编辑、取消、预算及有效运行时优先级。
- 原核65项回归在 `target/r03-regression-alias-red.log` 实际全部通过：IO8、R02配置11、pyproject4、R01context7/request16、tool-versions2、remote include4、旧锁路由2、profile模式3、conf.d2、ignore6。该组最后追加的别名新测试预期失败，因此整体exit101，不混称整组通过；别名已在最终58项中通过。此后这些原核未改，不重复验证。
- 首次格式检查仅发现新增reexport行需换行（0.20s），修正后定向检查通过；最终格式日志为 `target/r03-final-format.log`。
- 每组共同1200秒上限，完整日志保留，均未超时。最终lib-test36条警告、链接器提示及依赖未来兼容性提示保留，不执行cargo fix/加allow，不宣称全库无警告。
- 当前只证明macOS上的这些行为。Linux专用非UTF8测试未执行，实际安装/共享、生产CLI、事务、严格禁止atime变化以及跨平台矩阵没有冒充通过。

### 2026-10-18：R04 共享身份与选择

- `target/r04-model-red.log`：6项预期失败，66.75s；`target/r04-layer-red.log`：36项8通过/28失败，80.42s。身份/模型新行为各有真实RED；选择门面最初被身份桩阻挡，未伪称本层RED。
- `target/r04-facade-red.log`：40项32通过/8失败，77.07s；此时身份18项与模型/journal12项实际通过，选择门面8项到达自己的未实现入口。2项拒绝覆盖初次通过未声称RED。
- `target/r04-green.log`：44 passed /0 failed /0 ignored，76.33s，exit0。独立Python黄金digest、版本/预算与配置字符串覆盖首次通过，只记覆盖。
- 后续增加generic不得自动升级共享的已实现行为覆盖。`target/r04-regression.log`六组为selections11、model/codec37、catalog13、journal14、cleanup12、recovery26，合计113 passed /0 failed /0 ignored，32.24s，exit0。其中23项属于当前R04集合，另外90项为原逻辑回归；当前45项R04用例均有本轮实际通过证据。
- recovery的crash_child是父测试提供环境后启动的子进程夹具。外层显式--skip其无环境直接成功路径，父测试仍真实启动/校验崩溃恢复，不把夹具自身空运行计作通过。
- `target/r04-final-check.log`：R04源码定向rustfmt --check及git diff --check退出0，0.09s。所有执行组共用1200秒上限、保存完整日志，均未超时。
- lib-test38条警告及链接器、依赖未来兼容性提示保留；未加allow或自动cargo fix。仅本机纯模型、文件和受控子进程行为已验证，实际多项目Python payload共享、生产CLI、跨平台安装仍按后续任务验证。
