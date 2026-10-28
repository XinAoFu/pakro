# Pakro Python 完成计划：P2 之后

更新：2026-10-27（Asia/Shanghai）。架构入口：[主架构与Agent规则](Pakro主架构文件与Agent规则.md)。

本文件承接用户本轮确认的范围与实现计划；与此前“待确认”或旧范围表述冲突时，以用户本轮决定和本计划为准。接续检查已完成，P3、P4已由指定会话最终通过，当前按顺序进入P6；P6及后续新功能尚未通过。此前一次本地Git提交授权不延伸为后续自动提交或推送。

用户最新明确授权“后面测试你进行，我要睡觉了”：本轮后续编译及测试由Agent执行，覆盖此前长验证需用户终端的工作分配；每组仍限600秒，不提升资源额度、不启用通用自动多进程运行器，不恢复Git提交/推送授权。

用户已指定：按本计划顺序实施，每个P阶段完成后自动发送给“架构设计与代码审查”会话（`01a171b4-531e-7dd2-8ef0-4df8d6e97dd5`），由该会话核对实际源码和验证证据并明确通过后，才计为阶段通过；审查不通过则先修复，不跳到下一阶段。本轮不自动提交或推送Git。

2026-10-26最新范围：继续完成到P6后截止，不进入P8/P9。用户授权清理target无用编译产物，并使用本机SSH目录中的腾讯云密钥登录Linux服务器测试；仅使用本轮专用目录和进程，测试后清除本轮内容，保持服务器原有状态。新的复杂度、入口校验和函数组织规则已写入AGENTS.md；不把上一轮对推测安全场景的扩展自动当作本轮获准新增实现。

用户随后明确同意：先完成正常安装与取消、超时、Pakro意外退出的进程清理；保留文件/网络隔离，暂不为恶意主动躲避清理、攻击监督程序增加大型防御体系。此最新边界覆盖前轮将任意敌对后代绝对清理当作P6前提的解释，不撤销正常Ninja子组、生成程序登记、实际工具身份和失败不提交的要求。

本轮已清理当前有效四个根依赖图之外的2748个编译文件，约2.95GiB，target从9.0GiB降至6.1GiB；原日志/输入/当前1370单元均保留。清单及结果在target/p6-current-cache-cleanup-candidates-20261026.json与p6-cache-cleanup-result-20261026.json。腾讯云已通过现有SSH别名Home_server连接（不存密钥内容）；仅本轮目录为`/tmp/pakro-p6-20261026.lmAq9I`，.owner标记绑定本会话，结束必须删除。主机Ubuntu6.8、x86_64、2CPU/约4GiB、ext4；无全局安装或配置修改。实际无uidmap的`unshare --user --pid --fork --kill-child=KILL`可用，内部PID1；禁止把map-root-user失败当成全部命名空间不可用。

2026-10-27进一步完成逐文件缓存清理：保留当前main、普通Rootlib、CLI-test、util bootstrap/test及index六根和1685单元依赖闭包，删除47个明确被替代/旧仪表产物及3990个无当前引用的非诊断fingerprint文件，逻辑2272683913B、allocated2288205824B；根二进制/指纹SHA和候选dev/ino/uid/mode/mtime/ctime/nlink全部重核，未整删目录。SDK/输入/全部日志/diagnostic/debug-build/unknown均保留。证据target/p6-mac-current-cache-audit-20261027.json与p6-mac-current-cache-cleanup-result-20261027.json。另按receipt绑定清理6个失败SourceCli私有TMP夹具139376314逻辑字节，原目标输入/日志保留，见target/p6-source-cli-cleanup-result-20261027.json。腾讯云总job仍未最终清理（测试进行中）。

随后核对8个旧失败SourceCli私有TMP夹具的receipt、UID/0700/root inode/时间及无活跃进程，物理清理1129322868逻辑字节（allocated1198751744B），未续行旧删除op、未重写旧失败结论，保留所有target输入/原日志/receipt。候选与结果为target/p6-source-cli-final-cleanup-{candidates,result}-20261027.json。新测试仍新建隔离数据，不使用旧失败数据冒充通过。

## 一、目标、已完成基础与最终范围

### 当前基础

P1、P2 保持“已完成”的状态，不重新开展原阶段。后续工作复用已有的：

- 精确 Python 安装、目录选用和共享内容。
- wheel、限定纯 Python 源码包的安装与构建。
- 项目关联、查询和指定对象删除。
- 统一 `pakro.lock`、严格恢复。
- 锁文件与项目记录的受控提交、中断恢复。

此前旧代码删剪只做过静态检查。实施新功能前，应完成删剪后的编译及必要定向回归；这属于接续检查，不改写 P1/P2 的历史完成结论。

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

**实施状态（2026-10-25）：接续检查完成，退出条件已达到；不重开原P1/P2阶段，不表示P3通过。**

- 用户终端本次构建日志：`target/p2-manual-build-20261025-174510.log`，退出101；CLI模块未导入`SettingsExt`，四处trait方法无法调用。已补导入，第二次主程序构建已越过该错误。
- 第二次构建：`target/p2-manual-build-20261025-175050.log`，主程序构建成功，库测试编译退出101；发现测试仍调用已删除的history解析入口和兼容目录发布函数。已保留旧缓存/克隆与真实文件观察隔离的测试，移除退役兼容目录的三个用例，保留同文件内归档、来源和冻结设置的用例；第三次构建已通过。第二次串行命令在构建失败时停止，后面的exact/regression当时没有执行。
- 更新现有精确解释器安装用例：首次项目安装应生成P2精确锁并保留原生未知内容，重复安装才要求锁不变。
- 在现有共享wheel验收中补充项目查询与真实catalog/使用者一致、引用中精确删除拒绝、离开项目撤回终端选择但保留登记，以及这些操作不下载、不改变内容和关联的断言。本次regression已实际通过。
- 删除仅适配已退役任务展开/非Python安装路径的两份旧CLI验收；P1手动入口改为当前精确Python安装与共享项目检查，不恢复旧产品功能。
- 本次静态检查：修改的CLI模块rustfmt检查、三个修改Shell文件的`bash -n`及`git diff --check`退出0。不复用旧构建日志作为本次通过证据。
- 两个库测试文件的rustfmt检查及两个Shell用例内4段Python的AST解析退出0；原生文件观察测试不再调用history入口，归档/冻结设置检查继续保留。
- 根据用户本轮清理要求，移除已确认依赖退役history/task/opentelemetry代码的旧Pakro主程序、库、测试产物及对应fingerprint，实测释放2.287GiB，target从9.096GiB降至6.810GiB。当前主程序/库及依赖构建缓存、原始P1/P2证据、ISAT有效输入与本轮日志均保留。清理明细：`target/continuation-target-cleanup-20261025.json`。
- 第三次用户终端构建：`target/p2-manual-build-20261025-175650.log`退出0；主程序、lib测试和CLI测试全部构建成功。精确解释器冷安装与重复复用用例：`target/p1-recheck-manual-exact-20261025-175818.log`退出0，1个实际e2e用例通过，核对原始制品、运行版本/前缀、首次精确锁及重复复用。
- Agent使用本次生成的CLI测试二进制执行保留入口解析/退役入口拒绝/恢复入口识别3例，以及命令效果标注1例，均退出0、无ignored。日志：`target/continuation-cli-parse-20261025.log`、`target/continuation-cli-effects-20261025.log`。这4例只证明解析和命令表契约，不代替产品行为验收。
- 用户终端`target/p2-manual-unit-20261025-175910.log`退出0：P2父用例31通过/1个辅助ignored；随后locked_graph组21通过、真实项目门面1通过、文件事务组20通过、悬空链接事务组7通过。各组有重叠与helper，不相加宣称独立行为总数。
- 用户终端`target/p2-manual-regression-20261025-180710.log`退出0：待恢复只读保护、真实共享wheel/项目查询/引用拒删/离开撤回、deps安装不改声明，共3个e2e用例通过。`target/p2-manual-wheel-20261025-181003.log`退出0：1个严格锁CLI用例验证冷暖恢复、不重新选传递版本、暖恢复HTTP为0、声明不匹配拒绝且保留原锁与选择。
- 接续检查源码证据：`target/continuation-source-manifest-20261025.json`（源摘要`a10b489209923fc1fdff386d9f3e04bbec4c64b4d0ff8ca3fb20c8c4d33e6bbf`，排除进度文档），对应差异`target/continuation-source-20261025.patch`；检查结束时已复核源码未变化，之后的P3改动不在该证据内。实际命令、日志SHA与结果汇总：`target/continuation-result-summary-20261025.json`。上述证据限固定macOS arm64与当前夹具，不扩大为平台矩阵或完整ISAT验收。

**交付内容**

1. 编译当前主程序、库测试和CLI测试。
2. 修复旧代码删剪造成的悬空引用、配置字段、命令注册和生成资源问题。
3. 定向验证当前Python安装、查询、目录切换、指定删除和P2锁恢复入口。
4. 核对现存验收脚本与真实命令一致，删除或更新只测试已取消行为的旧断言。
5. 不恢复旧功能来让旧测试通过，不扩大为重新实现P1/P2。

**退出条件**

当前源码能够构建；保留入口可实际到达；清理引起的回归已修复。原有证据与本次新结果分别记录。

### P3：配置读取、显式更新锁和项目使用记录

**实施状态（2026-10-26）：指定“架构设计与代码审查”会话最终明确P3通过，范围为本机macOS arm64既定P3功能。首轮四项及新增缓存校验问题均已关闭；不扩称其他平台、完整ISAT或后续阶段通过。**

- 已落地待验证：统一Pakro字符串/表声明及完整extras/marker意图；退出用户pyproject/requirements观察而保留第三方归档解析；显式Update与普通严格恢复分离；旧版本/制品软偏好、原源码构建输入复用；空依赖语言-only提交；精确ID forget及全项目初始状态比较、防旧进程复活和稳定锁保护。
- 安装协调器、锁文档保留、实际目标marker与CLI已完成首轮集成，待编译及实际行为验证。旧偏好来自原已提交图，解释器变化时仍由新目标筛选兼容旧包；旧事实只读核验可用于预览，不创建锁guard。未将子Agent源码总结、测试存在或格式器通过当作功能验收。
- 新增P3用户终端入口`tests/scripts/p3-manual-check.sh`，实际命令与用例待源码集成检查后执行；继续复用同一target/features/debug0/incremental0，不启用自动多进程运行器。
- 为实际验证解释器切换，取得官方PBS **CPython3.14.9+20261021**、`aarch64-apple-darwin`、`install_only_stripped`，26,705,294字节，SHA256 `9f8c3480b8f0db553c270db548e8a21ae8e39df412e5661d66e8050c5c6ed2a8`。官方release API digest、同release SHA256SUMS与本地流式hash一致，获取/核验组71.41秒、退出0；尚未执行该解释器。原始证据和归档位于`target/p3-runtime-inputs/`，保留为有效输入，不纳入编译垃圾清理。
- 首轮静态检查：修改Rust文件rustfmt解析/格式化退出0；P3手动入口、E2E入口及修改的deps脚本`bash -n`退出0，P3 fixture和3段内嵌Python的AST、Schema JSON解析通过；修正CLI文件末尾空行后`git diff --check`退出0。详细结果`target/p3-static-checks-20261025.json`；编译前源码清单及差异`target/p3-prebuild-source-20261025.json`、`target/p3-prebuild-20261025.patch`。尚无P3行为通过结论。
- 首次P3用户终端构建：`target/p3-manual-build-20261025-185538.log`，21秒、退出101、前后源码一致；一处输出宏传播I/O错误而函数返回unit。已改为Result传播，并在输出失败时明确说明锁/选择已提交，待重新构建。
- 集成预查还修正Update待恢复记录的准入：只读识别属于本项目的真实journal，实际执行先进入既有受控恢复，随后无条件重新完整校验；预览仍只读报告待恢复。新增真实结束进程后重新执行Update的三个提交位置测试代码及journal身份/占锁测试，尚未运行。schema1 journal与project_state v1算法保持原样；原journal没有跨进程原配置指纹，恢复handoff核对的是本次调用持有的非锁输入及原文档/选择事实，不将其描述成额外持久证据。
- 上述修正后6个变更Rust文件格式解析、手动脚本Shell语法及diff检查退出0（`target/p3-format-recovery-20261025.json`）。P3构建入口改为先编译lib测试、CLI测试再构建主程序，以尽早暴露测试调用面的类型问题；仍是同一target和构建配置。
- 第二次P3用户终端构建：`target/p3-manual-build-20261025-190908.log`，129秒、退出0；lib测试、CLI测试和主程序全部生成。`P3_SOURCE_UNCHANGED=true`，前后源码清单`target/p3-manual-build-20261025-190908-source.json`及`-source-after.json`相同。本结果只证明构建，不计为P3功能通过。
- 首轮P3用户单元：`target/p3-manual-unit-20261025-191334.log`，153秒、退出101、源码一致；41通过、2失败、1个子进程helper ignored。恢复测试未传原中断开关要求的`PAKRO_TEST_P2_LOCK_CHILD`，未触发要求的真实退出；已修测试接线。同锁/别名测试不必要地重叠持有深目录观察；原编译测试单跑可过，在独立诊断进程将soft FD限额降至256后可复现观察失败，已缩短各观察生命周期，不提高系统额度、不弱化生产保护。原始诊断日志在`target/p3-admission-*-20261025.log`与`target/p3-two-failure-order-20261025.log`。
- 此次只改两份失败测试及手动入口的定向选择，生产源码与首轮单元一致；差异核对`target/p3-recovery-fix-scope-20261025.json`。原41例中的未受影响源码构建等结果保留，不无故重跑。新增手动`recovery`组只复验真实中断及同锁准入路径，并首次执行CLI的P3单测；其后仍须执行真实`lifecycle`组。两个Rust测试的格式解析、Shell语法和diff检查退出0。
- 用户定向`target/p3-manual-recovery-20261025-193256.log`中5个lib父用例实际通过、1个helper ignored，三个真实退出/恢复位置均有结果，用户soft FD限额为256；但脚本随后空数组展开触发nounset而错误报告exit0，**该脚本整体不计完整通过**。缺失的CLI P3用例已由Agent用本次二进制单独执行，1通过/0ignored/退出0：`target/p3-cli-admission-20261025.log`。手动脚本已改为兼容空数组，并增加完整步骤标记及cleanup失败状态，待核验修正后的完整运行。
- 用户真实生命周期`target/p3-manual-lifecycle-20261025-193350.log`，269秒、退出1、源码一致。已实际完成增删/改版、兼容pin保留/冲突调整、extras/实际目标marker、忽略用户Python文件、只读预览与准备失败保护、A/B隔离、CPython3.13.16→3.14.9真实切换、最后根释放及forget；重新登记操作也成功，随后测试误把旧3.13槽的最大revision7要求新3.14槽revision4超过，实际该槽由clear3正常增至4。已按完整selection key修正断言，不改生产版本语义。
- 只补尚未执行的移动尾部：`target/p3-lifecycle-tail-20261025.sh`绑定上述唯一保留的隔离夹具、原日志/目录/配置锁/源码/二进制SHA，先核验每槽递增，再新路径选用成功后忘记旧ID。原269秒与尾部共同600秒预算，尾部最多331秒；不再下载/安装Python，不重跑已通过前半。输入绑定`target/p3-lifecycle-tail-20261025-input.json`，执行结果将留`target/p3-lifecycle-tail-20261025-run/`。当前仅完成Shell/Python语法与diff检查，尾部尚未执行。
- 修正后的用户`target/p3-manual-recovery-20261025-200056.log`完整执行：7秒、退出0、`P3_GROUP_COMPLETE=true`及源码一致；lib父用例5通过/1helper ignored，CLI1通过/0ignored。此前193256脚本缺失步骤不再计完整通过。
- 首段尾部实际执行10.463秒：新路径已提交独立ID并在forget前实际导入，HTTP请求0；删除旧测试目录前保护检查因多出合法`.pakro.lock.guard`停止（未删除）。记录`target/p3-lifecycle-tail-20261025-run/receipt.json`，状态明确为不完整/退出1。
- 随后Agent在同一强绑定隔离根执行仅剩4个短命令及结果核对：精确删除两份原hash绑定测试文件及空、UID501/mode0600且独占锁成功的Pakro guard；缺失目录的forget预览/实际forget、moved路径查询和真实3.13导入均退出0，A/moved选择及两库完整内容/身份不变。4.387秒、退出0；原269+尾10.463+收尾4.387共同283.850秒。原始命令/输出/当前catalog及前后源码在`target/p3-lifecycle-finalize-20261025-run/`；不能将原失败全脚本说成单次exit0。
- 受影响旧内核短检查：`target/p3-retained-core-regression-20261025.log`在FD限额256下15通过/2失败、4.02秒。并发用例已实际得到一胜一负，仅旧错误文案断言不匹配更完整项目状态提示；手工构造journal的旧测试未建立生产前提的稳定项目锁。只修两处测试，生产保护不放松。增加`retained`手动组定向复验并发、原P2 journal/原生字段和只读锁承诺；之后补严格CLI冷暖恢复。
- 最后用户保留内核`target/p3-manual-retained-20261025-201949.log`完整17过/0ignored/exit0，56秒含测试编译、实际测试4.19秒；`target/p2-manual-wheel-20261025-202045.log`实际严格冷暖恢复exit0、124秒。Agent另补`target/p3-external-edit-and-package-crash-20261025.log`两父例通过、4.39秒，覆盖Pakro外部改动后的共同提交拒绝与带包joint commit真实进程终止恢复。
- 已自动向“架构设计与代码审查”会话发送当前P3源码与上述原始证据，明确并发目前是共享提交内核证据，最后双完整CLI竞争补交前不能宣称阶段通过。当前审查入口`target/p3-review-evidence-20261025.json`、`target/p3-review-source-20261025.json`、`target/p3-review-20261025.patch`；后续正式结论和必要修复只回写本PLAN。
- 实际两个完整`install --update-lock` CLI竞争由用户终端完成：`target/p3-concurrent-update-20261025-run/receipt.json`、`terminal.log`，9.184秒、exit0、complete=true。lsof证明两个自有PID均已打开同一非继承原guard、捕获原项目状态后才释放；两Update码0/1，失败为原输入失效，HTTP0，A只增加一次revision、锁/native/catalog一致、无journal、moved/历史B状态及两库内容不变。日志已自动补交指定会话。
- 资源证据限定：并发脚本的256是启动时继承soft值，既有未修改的CLI启动代码仍会将自身soft提高到最多4096（实际guard FD3051）；不能说CLI全程在256。P3未修改额度代码，也不将其当新增资源优化验收。库测试的FD256证据单独成立。澄清保存在`target/p3-cli-fd-evidence-note-20261025.json`，已随审查补交。
- 指定会话首轮源码审查明确不通过，原始结论`target/p3-designated-review-round1-20261025.json`。阻塞：P1完整extras根意图未被候选图复用验证（带extra项目可污染无extra项目并生成不可冷恢复锁）；P1 CLI反复丢弃/重建首次计划可跨forget重新基线化；P1旧源码记录直接rebuild而未先复用已发布完整输出，会使无关依赖更新因nonce输出失败；P2同一实际解释器的裸版本/精确build不同写法跳过锁而误报extras缺失。
- 按上述4项分工修复并补对应定向证据，保留此前真实通过与失败日志，不把原验证扩称上述窗口已覆盖。修复后自动向同一会话复审，通过前不推进P4。
- 首轮审查修复集成中：共享图复用按受保护包的实际METADATA重新推导完整extras/marker最小闭包；CLI保留首次Config、观察及全项目状态直到执行；等价解释器写法按实际实例与来源决定锁适用性。源码产物增加原ZIP完整核验复用及Ready包树复用；依赖事实变化时分配独立新树并重新生成脚本和RECORD，不重建或伪造原ZIP。新增对应库、CLI和真实冷暖用例，当前尚未编译或执行，不能引用修复前结果作为通过。
- 4项修复已完成源码集成，原ZIP和Ready两种来源均有独立受控能力；严格恢复缺布局时也可沿原锁复用完整包树，Ready损坏仍拒绝。查询另补同来源不同合法sysconfig配方与随机坏runtime key/target的区分。修复范围相对首轮审查为43个源码/测试文件，其中41个Rust文件格式解析退出0；Shell语法、fixture和3段内嵌Python AST、Schema及diff检查均退出0（`target/p3-repair-static-checks-20261025.json`）。当前仅静态检查通过，待用户统一构建及新/受影响定向行为验证；`source-retained`手动组只复验受影响的两个旧源码协调用例，不重跑无关长测试。
- 修复后的首次用户构建`target/p3-manual-build-20261025-214757.log`，25秒、退出101，源码前后一致且与修复集成快照一致；lib测试编译发现派生包树两个复制函数把`OperationError`直接返回给`eyre::Result`。已用`?`传播并显式返回unit，同时移除查询模块一处新产生的未使用导入；格式解析及diff检查退出0。与失败构建相比只改变上述两个文件，证据`target/p3-repair-build-failure-20261025.json`。CLI测试编译、主程序构建和修复行为用例尚未执行，待重新统一构建。
- 类型修正后用户构建`target/p3-manual-build-20261025-215143.log`，129秒、退出0，complete=true且源码前后一致；lib测试、CLI测试和主程序均编译成功，当前源码与类型修正快照相同。日志、源码和三份实际二进制SHA见`target/p3-repaired-build-evidence-20261025.json`；短只读注册检查发现55个库用例及5个CLI用例（含子进程helper，注册数不等于行为通过数）。修复后的行为测试尚未执行。
- 用户修复后定向组`target/p3-manual-unit-20261025-215811.log`，196秒、退出101、源码前后一致并与上述构建一致；库52通过/2失败/1helper ignored。真实原目标/输入复用用例及完整闭包、脚本重定位等已通过；新nonce整链子进程栈溢出，解释器不同配方夹具的sysconfig缺严格解析要求的头注释。先定位栈的结构问题并修正夹具；不提高栈、FD或超时额度。失败使CLI组和extras整组都未运行，不能计为通过。原始证据`target/p3-repair-unit-failure-20261025.json`；未受影响且已通过的用例保留，不无故重复长源码验收。
- 栈故障残留表明CPython已就绪、demo源码已解包、初始构建依赖锁已建立，尚未获取counter-dep；将定位收窄至首次源码构建递归进入初始依赖准备，不称为测试入口故障或已证实无限递归。现已将生产registry准备/求解、元数据等待、严格回放及递归hook等待体设置独立BoxFuture边界；测试改薄分发壳与盒化流程并记录阶段。原owner、输入、300秒测试deadline、预算、校验和重建条件不变；未提高资源额度。sysconfig夹具补原解析器要求的合法头注释。相对失败组只改6份源码/测试文件，格式、Shell/Python语法及diff静态检查退出0，尚未重新编译或复验；范围与快照`target/p3-stack-fix-scope-20261025.json`、静态结果`target/p3-stack-fix-static-20261025.json`。手动repair只补两失败库父例与五CLI父例并刷新主程序，严格检查注册数及零ignored；随后补两个受影响的旧源码用例和尚未执行的extras实际CLI组。
- 用户`target/p3-manual-repair-20261025-221937.log`，155秒、退出101、源码前后一致且匹配栈补丁；lib测试重新编译成功，外层两父例1过/1失败（日志另含nonce子进程失败摘要，不叠加计数）。查询用例实际通过完整不同拼写/合法配方/损坏key与target拒绝；nonce已完成首次实际源码安装，越过原栈故障位置，测试future大小15056字节，随后无关wheel更新时在完整inventory校验报EMFILE。启动softFD256、RUST_MIN_STACK未设置；未调整额度。正核对测试观察生命周期及生产Ready/layout保护的重复持有，按结构修复。五CLI、主程序刷新、source-retained、extras仍未执行。证据`target/p3-nonce-fd-failure-20261025.json`。
- FD结构修复已集成、待编译实跑：布局已有结果、发布成功及并发胜者三条路径沿用本操作真实language/package Arc保护，只为缺项及layout新取得保护；核对同库物理store/namespace、完整成员集合、实际manifest/全部内容/投影，跨库同key仍拒绝。old-key源码可提供新派生树，已有派生目标的早期查找不把旧源码误当布局成员；私有prefix接管kernel后及时关闭外层store观察。nonce只把原完整manifest/inventory/物理身份作为预期描述跨操作保存，重取实际lease完整验证，不长期额外持有纯测试runtime/package；原断言保留，损坏保护须命中明确内容错误，EMFILE不计通过。新增两项实际handoff/同库跨operation/缺项/派生暖复用/独立跨库拒绝小测。相对221937只改6份文件，静态格式、Shell/Python AST及diff退出0，范围`target/p3-fd-fix-scope-20261025.json`、快照`target/p3-fd-fix-source-20261025.json`、检查`target/p3-fd-fix-static-20261025.json`。repair当前固定3库父例和5CLI父例、零ignored并刷新主程序；已通过且未改的query用例不重复，随后补两个旧源码及extras组。未提升额度、添加缓存或改写持久schema。
- 已将上述冻结源码及全部4项修复、资源问题和证据边界自动发送指定“架构设计与代码审查”会话并行只读预复审；明确最新FD补丁尚未编译、行为日志需补齐，当前不是P3通过申请或通过结论。验证完成后补交同会话取得最终结论，仍不进入P4。
- 用户最新三组已完整通过且全部源码前后一致、匹配`f3a538cf…6c330`冻结清单：`target/p3-manual-repair-20261025-230218.log`302秒/exit0，3库父例及5CLI父例全过、零ignored，并刷新实际主程序；`target/p3-manual-source-retained-20261025-230720.log`259秒/exit0，2个受影响旧源码父例全过，含严格nonce输出拒绝；`target/p3-manual-extras-20261025-231139.log`263秒/exit0，真实同意图0HTTP复用、普通根不继承feature、A/B隔离、等价full-build/prefix查询及配置改动拒绝、普通/extra锁冷暖恢复均通过。各组独立少于600秒。实际日志、源码和三份二进制SHA汇总`target/p3-fd-repair-results-20261025.json`；保持原CLI启动FD额度说明，不把inherit256宣称完整CLI全程256。
- 指定会话预复审确认原四项修复及FD交接源码成立、无新确定交接阻塞，原始结论`target/p3-designated-review-round2-20261025.json`，但另列P2：低层严格恢复命中sdist输出ZIP缓存时直接`from_locked_output`，没有核实际source backend/重算request_id，可能暖缓存接受、冷重建拒绝同一篡改锁。已确认`locked_graph/prepare.rs`及递归`build_inputs.rs`存在同类入口；正统一交回实际SourceBuildSession与原锁slot校验并补暖/冷一致拒绝、lock/catalog不变的定向回归。已过组保留，不因新缺口重开其它已通过行为；补齐该缺口后仍需指定会话最终通过。
- 缓存窄修已集成、尚未编译实跑：runtime及递归build-input两处低层sdist缓存直收都改为NeedsBuild；实际SourceBuildSession从原ExistingFileSlot共用核对source/backend/初始声明及实际language/recipe/fixed inputs计算的request_id，暖ZIP和冷重建在hook前同样拒绝不符，输出ZIP/摘要/METADATA完整校验保留。新增实际demo+counter构建回归，有两入口合法暖复用零hook正例，再验证两入口×backend/request_id×暖ZIP/无输出缓存共8种精确拒绝，hook不增加、锁及真实catalog字节不变；旧低层假request记录用例改验必须交回实际续行。相对f3冻结只改7个Rust文件及手动入口，格式、Shell/Python AST及diff退出0；`target/p3-cache-fix-scope-20261025.json`、`target/p3-cache-fix-source-20261025.json`、`target/p3-cache-fix-static-20261025.json`。新增cache组只跑新父例、受影响旧低层父例和旧P2源码冷暖父例，共3父例零ignored，并刷新主程序；不重跑未受影响且已通过的CLI/extras/S3 fresh组。
- 用户`target/p3-manual-cache-20261025-234205.log`305秒/exit101，源码一致并匹配01a9缓存修复快照；两个受影响旧父例实际通过，新父例在初始化catalog时被library-only测试策略正确拒绝，还未执行新8组合，主程序刷新未运行。只为该fixture显式捕获正确的project_selection=true策略，保留其它权限、原300秒绝对deadline及全部断言；生产门和通用helper不变。相对该组仅`install/build/tests.rs`变化，证据`target/p3-cache-policy-failure-20261025.json`与`target/p3-cache-policy-fix-source-20261025.json`。按用户最新授权，Agent接手后续，仅重跑未过新父例并刷新主程序，已过旧例不重复。
- 10月26日Agent已完成补验：`target/p3-agent-cache-case-20261025.log`实际新父例1过/0失败/0ignored，测试95.25秒、编译49.14秒；`target/p3-agent-cache-main-20261026.log`主程序构建46.21秒/exit0。该Agent组UTC16:00:49起至16:05:16确认结束，保守共同墙钟267秒，低于600。前后源码一致，摘要85190173…b1af；相对已通过代码预审的01a9版本只修测试setup权限，生产实现未变。旧cache整体exit101仍保留为失败记录，两个旧父例的实际通过与本次新父例通过分开计，不伪称一次全组exit0。日志、资源、源码、lib/main SHA见`target/p3-cache-final-results-20261026.json`；空的自有临时根已rmdir。准备补交指定会话最终判断。
- 已自动向指定会话提交上述完整证据请求P3最终审查，当前最终审查turn=`01a173d2-0ddc-7e32-9624-ca6ed1429a44`运行中；851冻结源码保持不变。此前01a9缓存源码预核已明确无新阻塞，原始结论`target/p3-designated-review-round3-20261026.json`；尚待实际补验后的最终明确通过，P4仍未启动。
- 指定会话最终turn=`01a173d2-0ddc-7e32-9624-ca6ed1429a44`已结束，明确“P3通过”，复核851冻结源码及最新main SHA匹配，接受缓存两旧例与修复后一新例分段证据且保留原exit101失败记录。原始最终结论`target/p3-designated-final-approval-20261026.json`。仅据此把P3计为通过，并按既定顺序进入P4；没有提交Git。

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
6. 提交前重新核对配置、锁、对象及项目版本。
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

增包、删包、版本变化、解释器变化、最后一个包移除；兼容旧依赖保留、不兼容旧依赖调整；A更新不影响B；准备失败、外部改文件、并发更新、中断恢复；目录已删除仍能forget；旧进程不能复活已清空关联。

### P4：指定删除完善、显式回收与中断续行

**实施状态（2026-10-26）：指定“架构设计与代码审查”会话最终明确P4通过，范围为本机macOS arm64既定功能及隔离数据验收。原阻塞和后续分类、原因链问题已关闭；不扩称Linux或后续阶段通过。没有对用户实际库执行回收。**

- 只读核对已发现现有EX/完整inventory/FD捕获可复用，但没有持久删除进度；包删除仅撤关联layout Ready而保留目录，旧无清单孤儿不能据此补造删除许可；现有reserve/publish需同库pending claim门阻止未完成删除时同key重发。
- 按现有架构文件的P4约束分工实现统一图视图/有向工作组、共用持久删除及续行内核、catalog-only CLI与准确结果。不可变原manifest和短进度分开存，限定整组实际保护容量，不通过提高句柄/超时或拆环完成。当前尚无P4编译或行为通过结论。
- 用户此前target维护要求已继续处理：只读审计target约7.9GiB，当前main/test两套依赖图、有效runtime/ISAT输入及原始证据均保留。按逐项SHA、inode/uid/mode/mtime及当前Cargo输入匹配，删除64个确定退役的opentelemetry/已删旧功能产物与旧Pakro检查元数据，实占释放122.00MiB；不删除候选外内容、历史util或原始日志。明细`target/p3-obsolete-build-candidates-20261026.json`与`target/p4-target-cleanup-20261026.json`。这是工程构建缓存维护，不当作P4产品回收验收。
- P4第一版图层/CLI已接线并补定向测试，删除核心及Preparing/Complete记录正在最后集成。首次诊断`target/p4-first-typecheck-20261026.log`观察墙钟53秒/exit101，仅报对象probe闭包同一错误类型推断的4条诊断；该轮明确为未完全冻结的类型诊断，不计P4构建/行为通过。新增受限`open_reclaim_journal_writer`与`forget_reclaimed_retired`，只用libraries权限做journal/受限退休CAS，维持project_selection=false，不能取得可变项目关联模型。两类manifest复用既有data-only合法性检查，避免图去重掩盖坏边。
- 全源e426fd53…62312冻结后构建lib/CLI/main均exit0（47.68/41.55/40.50秒），前后源码一致；原第二轮28条unreachable_pub诊断已收窄内部可见性解决，不放宽lint。首次P4行为`target/p4-unit-lib-20261026.log`51.04秒/exit101，22父例通过/3失败/2辅助ignored；`target/p4-unit-cli-20261026.log`0.04秒/exit0，7父例全过。27库注册不混算为27通过，前后源码仍e426。两个图fixture发行名下划线归一化不符已仅改为canonical名称，生产门不变；真实退出续行在payload-intent时错误重查已完成上层组、引出已撤Ready语言的假dangling边，正按实际cursor终态修复。汇总`target/p4-first-behavior-results-20261026.json`，尚无P4通过结论。
- a97b9255…e659三文件窄修保留完整实际EX/原manifest/cursor与物理缺失检查后跳过整完成组旧live边；不松未完成组/外部未知边，报告去重。`target/p4-repair-three-20261026.log`编译成功后2过/1失败，53.75秒：两个fixture例实际通过，原payload位置已越过；更后cut=complete暴露只读Store缓存未满足后续claim写请求。before/after清单一致（`target/p4-resume-fix-source-20261026.json`及`target/p4-repair-three-after-source-20261026.json`），主程序刷新尚未执行。
- 指定会话P4预审明确2项P1、1项P2，原始结论`target/p4-designated-prereview-round1-20261026.json`：Complete读→写能力交接；已发生pending动作恢复时漏父fsync；受保护语言只解key不校完整manifest。第一项已按原anchor/物理store+build身份核验后open_existing升级，不初始化或猜路径；后两项正在统一physical/metadata parent sync及完整data-only语言清单核验，并补精确负例。已过P4库/CLI例保留，后续只补未过及受新修影响项，仍需指定会话最终通过。
- 三项预审窄修已冻结、待编译实跑：Read→Writer cache只经原anchor及新旧root/build同inode核验后open_existing升级；capture/entry/payload/allocation所有存在或缺失分支在done前同步原父，Ready/claim namespace及中央进度读取也补sync。新增6类syscall成功但父sync前真实退出+新进程续行、实际原父inode sync证明（另核中央sync），不是只检查路径消失。语言即使被项目保护仍完整解码既有manifest规则，新增仅key/schema99两种损坏会阻止其它候选删除的父例。另新增真实main A/B共享/forget/prune/completed-resume验收。相对a97只改10文件，格式/diff退出0，冻结`target/p4-prereview-fix-source-20261026.json`（0b7b4b1f…ac423）；根开始编译与定向行为复验，尚无通过结论。
- 0b7冻结lib与main再次构建exit0（49.92/42.02秒），源码保持一致。`target/p4-targeted-seven-20261026.log`86.30秒/exit101，6过/1失败/0ignored：实际main A/B共享保护、双forget后回收三对象、completed resume预览/重复幂等；原全部durable退出续行；六类sync前退出及实际父同步证明；claim/新实例保护；两种已选语言坏清单拒绝均通过。仅新store升级负例的外来库绑定失败被误分为Damaged而非Unknown，仍拒绝未取得权限。正以typed控制库绑定错误收窄分类，不改内容损坏分类/测试期望；已过6不重跑。原始汇总`target/p4-targeted-seven-results-20261026.json`。
- cd84纯控制库绑定typed分类修正后，新upgrade父例1过/0ignored（2.13秒，编译47.79秒），main构建exit0；6个受影响旧f4精确卸载例5过/1失败（4.01秒），唯一为保护正确但丢失人可读parent→dep原因链。仅prune/graph两文件补迭代真实路径输出、资格与目标不变后，`target/p4-reference-chain-regression-20261026.log`余下一父例1过（2.08秒/0ignored，编译47.77秒），main再次构建42.15秒/exit0。最终fresh main又运行真实A/B共享→forget→prune→completed resume父例，9.40秒/1过（`target/p4-final-main-lifecycle-20261026.log`），并核main SHA。
- 当前全部30个P4库父例、7个CLI父例及6个受影响旧精确删除父例已按明确版本分段取得通过；2个ignored仅专属子进程helper。原全组失败日志仍保留，不伪称一次全量exit0；完整scope矩阵及SHA见`target/p4-final-verification-20261026.json`。最终源码5a55de45…9905前后无漂移，`target/p4-final-source-20261026.json`，准备提交指定会话最终审查；尚未计通过，未进入P6。
- 已自动补交全部P4原始日志、分段scope矩阵、最终源码及main SHA，请求指定会话最终明确结论；当前审查turn=`01a17462-4848-7d93-afd8-39d6006f01b9`运行中。此前三项阻塞的0b7代码复审已明确修复成立、无新确定阻塞，后续只收窄控制库错误分类和恢复人可读引用链；当前仍等待实际证据后的最终通过。
- 指定会话最终turn=`01a17462-4848-7d93-afd8-39d6006f01b9`已结束，明确“P4通过”，确认5a55源码/main摘要、30库父例/7CLI/6旧精确删除的分段证据和真实主程序生命周期，未混算helper/重复运行，原失败组保留。原始最终结论`target/p4-designated-final-approval-20261026.json`。仅据此计P4通过并进入P6；未提交Git。

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

**实施状态（2026-10-28）：P6完成。Mac与Linux六原生后端各6/6全过，源码CPython完整生命周期两平台均实际通过；用户已取消指定审查会话签字环节，以实际验证结果为准。完成到P6即截止，P8/P9不进入。仅支持用户确认的正常公开后端及取消、超时、Pakro异常退出，保留文件/网络隔离，不扩建恶意主动逃逸体系。腾讯云专用目录及fixture待清理。**

本轮用户批准并实施的三项改动（2026-10-28）：

- 删除操作时限与查询分离。`src/core/programs/library.rs`原将30秒单一期限用于全部CLI操作，删除大型CPython需被切段并反复`prune --resume`。现查询保持30秒，用户显式删除/回收取300秒。删除内核的逐文件父目录fsync与进度记录未改，P4持久化合同不变。
- 查询不再遍历哈希整棵已发布树。`which`/`env`/`installs ls`原经`check_binding`→`check_full`→`observe_tree_inventory`哈希全部约3000个文件，且`acquire_existing`收尾与查询各做一次。现查询侧新增`check_metadata`：Ready清单字节、物理绑定（inode/属主/权限）、入口解释器单文件哈希。安装、发布、包物化、删除仍走`check_full`，未削弱。Linux实测每次查询约9秒降至约1秒。
- 原计划拆分Source父用例（C项）未实施：上述两项改动后整组已在580秒内一次通过，拆分失去必要性，证据形状保持原样。

实测结果（两平台均`reclaim_resumed=false`，即删除一次完成、未走续行）：

- Mac `target/p6-round19-mac-source-lifecycle-20261028.log`：1父例通过、0失败、0ignored，309.499秒；冷编译210.032秒，暖复用5.564秒，查询0.171–0.287秒，删除85.260秒。
- Linux `p6-round19-source-lifecycle-20261028.log`（腾讯云，源SHA见`target/p6-round19-delta-20261028.json`）：1父例通过、0失败、0ignored，526.831秒；冷编译394.291秒，暖复用27.355秒，查询0.848–1.068秒，删除59.876秒。改前同用例为577.192秒被580秒上限杀死（exit 124），查询各约9秒，删除两次30秒超时后仍未完成。
- Mac六后端父例与Linux六后端父例此前已分别通过，本轮改动只触及查询与删除时限，未改构建路径；上述Source父例覆盖真实编译、七必要功能、全部已安装扩展、pydoc/idle、A/B同实例共享、严格暖复用零HTTP、只读不改状态、受引用拒删、forget、精确删除与物理回收。

指定会话历史审查结论（P3/P4）保留为记录，但用户已于2026-10-28明确取消该签字环节，P6及后续以实际验证结果为准，不再等待指定会话结论。

前三项清理修复已由指定会话turn `01a17a7a-7455-7233-a324-02740cb49283` 只读复核源码接线接受，尚待两平台行为：Linux独立watcher成为subreaper，停止和正常出口都以ECHILD确认收养子进程清空；Mac两次枚举严格大于0；全部生产session使用start_owned，spawn后首await前移交，Finish成功确认才收回。RawSourceTree、嵌套prefix、LanguageBuildOwner均不抢先chmod/删。新增两父例用真实生产对象和run future，手动配对C worker做确定的析构刺激，不冒称完整hook链；ready失败只证明保守保留，不算自动清理成功。静态检查target/p6-native-cleanup-static-20261027.json。

后二项增量复核turn `01a17a7f-de59-70d0-bc8f-0eaebe4b7733` 接受python-config.py分类/适配源码，普通Python转接还需补默认stdin（无代码参数或只有-u/-O）及保留原-I/-P所决定的cwd/脚本搜索语义，正补最窄入口与实际worker用例；不是通用进程虚拟化。Source冷重建测试设计可接受，一次执行只记录实际命中的匹配或拒绝分支，不同时宣称两者均验证。Linux eae40新epoch主程序175.209秒编译成功，库测试组完成前保持生产源不变，随后才同步本轮hook修正，避免跨版本冒称同一批通过。

最终hook `0ff1f10960dd8e6bed6584dd62c9694ef0dd04b0eaee636ec3ea5ea29ed5e6da` 已由指定会话turn `01a17a8d-8d5e-7273-8a4a-60c7a4f8d272` 复核两个入口源码关闭，配置脚本摘要相符，无新确定遗漏，待真实worker/Source冷重建。Linux eae40旧epoch主程序和库测试均编译成功；首次新析构组53.861秒真实1过/1失败：语言owner已通过，Source用例只在准备后期限过期、未进入析构刺激。仅外置测试改为先完成runtime构造/准备，再各case持新30秒Context和真实新LanguageLease，不增产品或整组上限；原失败保留，待新ac1b46 epoch实测。

新ac1b46 Linux main166.831秒/lib256.659秒均编译成功，前后源码一致；utility6父例全部通过（含owner SIGKILL/timeout/cancel/gate/wrapper/文件网络边界，整组60.916秒），生产对象析构2父例全部通过（行为53.13秒/整组57.024秒，含Source四刺激），日志target/p6-tencent-round12-{utility-cleanup,native-cleanup}-20261027.log。Linuxumask077本就创建0700；Mac后补的一行外置fixture显式chmod0700未混入该epoch，生产字节相同，当前C++实际验收进行中。

Mac新main47.44秒/lib55.50秒编译成功，config metadata短例1过；初Source析构夹具0755被生产validator拒绝、初nativePython夹具hyphen名生成非法wheel均未进入目标行为，保留原失败。仅补外置fixture显式0700及合法p6probe包名，随后target/p6-mac-review59-fixture-fix-{lib,source-cleanup,python}-20261027.log整组122.825秒退出0：lib51.27秒，Source生产析构四刺激父例1过7.93秒，真实nativePython父例1过60.58秒；语言owner此前已过不重。Python用例实际固定三wheel、全普通入口及-I/-P搜索选择、user site禁用、越界读写/联网拒绝和登记门均过。Mac utility刷新及Source按锁冷重建仍待，不计P6阶段通过。

Mac当前监督utility6父例已全部通过，含normal/owner SIGKILL/timeout/cancel/gate/文件网络隔离，1专属helper ignored不计父例，刷新编译与验证整组40.254秒，证据target/p6-mac-review59-util-*.log/process。Source完整按锁cold父例target/p6-mac-source-locked-cold-fixed-20261027.log/process实际427.251秒/退出0/1父例0ignored：A真实build212.487秒，B全新data/cache同官方raw/工具SDK/recipe和完整原锁真build208.770秒，自然实际输出从881fbbd9556370f73ba1d36a0419f1bad847ed2582a25074defef36647301479变为5c9799fa3f07a52560ee1373dc0cc79d009cc16b18caefd4df1379b20fdaef91，唯一输出gate准确拒绝，B无Ready/selection，A完整context snapshot及AB锁/声明字节不变。只证明实际cold重建与输出变化保护，不称匹配冷恢复成功。A直接执行config-*/python-config.py --includes退出0，两项均最终payload/include路径；逐命令原始证据target/p6-source-cold-cli-01a17a9a0265721699022e1b1e9f7b28。

Linux C++ round12父例480.92秒/整组484.953秒失败保留：实际try_run/AB导入共享/warm0HTTP及真正同锁冷重建的digest、buildrecord fingerprint、builds1、ZIP存在均通过，随后changedCC两种ZIP状态拒绝检查之一触原operation deadline，不能算完整父例或变化工具链gate通过。只读审计最窄耗时/夹具问题，不增480/580/600或句柄栈额度、不原样重跑。LinuxSource独立真实CLI首例正在运行，服务器总job尚保留，P6仍未最终通过。

指定会话turn `01a17aa4-a820-7592-adf9-c593cd558a6c` 接受C++真实耗时路径仅两行内部去重，已实施：私有reusable_published_package入口及rebuild入口复用公开prepare_registry/replay_registry已经完整验证、持续持有的同一LanguageLease，改用现有check_held_binding。原Context/deadline、实际SH/FD/原manifest仍核；公开入口、成功复用/缓存出口、Source session准入、Completed/publish/into_parts、resolve和最终提交全验不变，无新缓存/开关/额度。仍需原480/580/600完整实际父例通过，未以改Context期限绕过。

LinuxSource首次真实CLI父例在configure前被输入根上限拒绝（test31.68秒/整组36.185秒），不计源码构建通过：已有hostCC与私有CXX分属两个prefix，canonical21根输入形成9覆盖根，超过8，原失败保留。未提高阈值或删必要输入；仅私有解包官方同版本cpp包的cc1组件，Source改用同prefix GNU13真实CC/CXX，42探测项去重18、覆盖根8，实际C/stdarg/stdio与C++/vector/iostream编译运行均42（0.506秒），164项原静态SDK字节不变，无组合大SDK或系统安装。记录target/p6-tencent-round12-source-shared-gcc-complete-inputs-20261027.json。round13将同步两行代码并先Source真实验证，再原预算C++；服务器保留至完成。

Mac review61两行去重后main43.662秒/lib52.710秒及完整C++父例137.635秒全部通过，整组234.007秒、1行为父例0ignored，原480 operation不变。真try_run、AB同锁warm0HTTP共享、包cold产物/记录精确相等、changedCC ZIP有/无两状态预hook拒绝均过。当前main SHAca762aa562679728415c9c1877e8e7c8cedc5e894e921a19adc3e05c012d661a、lib cfa117da40d229a9b3348297eb2736259919ec7d3e98b740cc849ca4739fc4fa；target/p6-mac-final-verification-matrix-v3-20261027.json SHA2d8fc66d8ad7ac69c07a90dbc487d98833d2a6e4858382dab294fcf69476f02d链接原矩阵及v2，旧scope不倒写为新binary通过。target/p6-mac-current-cache-audit-v3-20261027.json重核当前六根1685单元、缺依赖0、可确认新增无用deps/fingerprint候选0，未删除有效输入和日志。Linuxround13仍在执行，P6等待全部结果、服务器清理和指定会话最终结论。

指定会话turn `01a17aaf-21cd-7e40-ac84-14af936821c6` 已只读局部接受Mac v2五项修复/Source实际冷重建拒绝和v3两行Cpp增量，未发现新增阻塞，不计P6整体通过。为使两项运行绑定收据自包含，当前同binary仅补utility6父例和nativePython1父例（83.357秒均过，helper单列），现场捕获真实env/cwd/argv、主程序和测试程序、原资产manifest/三wheel/PBS的stat与SHA前后不变，源码含docs也前后一致；旧收据env/hash缺项保留不补写。新target/p6-mac-final-verification-matrix-v4-20261027.json SHA7227eb0cc0ce37b8d53fc9cbb5b1b17c239cb21bda600d745c6fb9028154913c，现场收据target/p6-mac-current-runtime-binding-process-20261027.json SHA5f9442d3513da0d1a4b1eb3f766adf8fc2fd382b6cde94e3f1ac207aed59d2d6，原/v2/v3矩阵字节不变。Mac全部验收已完成，余Linux实际Source/C++及清理继续。

指定会话turn `01a17ac1-6093-76c1-92f8-97a2a6db45bb` 已核v4的17份关联文件、运行绑定/前后摘要/退出与父例计数，相关证据缺口关闭，不重复累计能力。Linux round13 main168.832秒/lib254.207秒均编译0、faf12c30源码前后一致；同GNU13输入的Source第二次在configure前被host LTO库链接逃出冻结根拒绝（test16.73秒/整组21.226秒），非实际构建通过。只读枚举确认34,246项/407链接中仅2条指向同一host /usr/libexec LTO文件，不是GDB插件。未删链接或放宽校验；新owned组合SDK按原静态164项及真实GNU开发布局、pkgconf和实际runtime链接目标准备，原输入/两次准备失败收据保留。最终1179个regular文件/217572019字节、链接完整、5覆盖根、真C/C++42、pkgconf独立RPATH及ELF检查通过，receipt SHA31427789cec62b74f78889322f6b94b84f4ca8f8c5252ae5dd3bf2c98002d5be。实际组合输入Source CLI父例已启动（target/p6-tencent-round13-combined-source-cli-20261027.log），未预记通过，结束后才串行Cpp最终父例。

组合SDK Source实际通过官方configure，但make首对象因sh EINVAL失败（167.87秒行为/171.595秒整组），未计完整Source通过。真实同监督诊断确认GNU Make4.3的RESETIDS问题：外部有效UID/GID下spawn/spawnp均成功，native未映射namespace中普通flags12均成功，加RESETIDS的flags13均EINVAL22。仅Linux ignored诊断父例，不计新backend能力；未改gate/attrs/身份或隔离。官方GNU Make4.4.1归档SHA dd16fb1d67bfab79a72f5e8390735c49e3e8e70b4945a15ab1f81ddb78658fb3、GNU keyring验签通过，私有新Make字节SHA d64ec7984bf83115b3f8e7babdd98d6d3526c28cd888aaa619f59127bc268901，原4.3/构建命令与smoke夹具失败均保留。

Linux两行去重后的Cpp仍在旧共享480秒的changedCC末尾失败（480.94秒/483.849秒），未计通过，不原样重跑。指定会话turn `01a17ada-e2f4-7871-9bf1-3b8e19adf6d3` 已认可固定私有Make4.4.1工具范围及真实独立请求夹具合同。仅native_tests.rs（SHA b3a638de989ebfae5f95a2f7d95e5d70257e67453535102aed0f79fc4b506a00）调整：原A/B/cold/common全验仍480并checkpoint，释放旧拥有对象后，尾部两次changedCC各重新完整取得Context/租约/锁/registry/预算，单次min(now+480,parent580)，父580始终不重置，末轮再完整核AB事实及实际导入。全部原拒绝/零HTTP/hook/锁/catalog断言保留，旧失败不改绿，不宣称旧共享480全流程完成。两平台增量lib与该完整父例正在验；Linux round14同步仅两测试文件，生产与main仍faf不变，随后新Make真实smoke/Source/Cpp串行。

后续Linux新合同首次common472.855秒已过，tail因shim误用CXX而更换CC安装布局触SDK根错误，未过；仅四行测试改为委托原CC（a230f4bf…22423）保留单变量后，Mac v6已完整通过131.59秒并获指定turn `01a17b01-1428-7c43-b182-1a09d6a13cb0` 局部接受。Linux原a230父仍在控制层577.195秒超时：common477.023秒、with_zip=true精确门61.040秒通过，false未完成，完整仍失败。同期Source OS-ABI真实首次install386.081秒、七功能/全部扩展、B暖装/查询及引用拒删通过，物理回收尾部在577.193秒父限超时；后态3034项物理完成仅说明最终状态，不证明原父期限内完成，不改绿、不续跑旧tail。

指定turn `01a17b21-e8c9-7213-8105-e4d9852861b9` 接受最窄已确定失败早拒及Source测试helper去重。现生产仅既有真实程序SHA与已验证expected同role必不等时返回原工具链错误，把SDK全量hash置后；None/match/missingrole保留完整SDK/config/NativeRecord/全部公开Runtime和提交校验，无新增hash/cache/权限/额度。Native适配代码与重编bootstrap导致新请求身份，旧epoch不倒写。Source helper只删除acquire_existing返回后立即重复的check_binding，该入口返回前已完整核验，同SH/FD及全部字段断言保留。

新epoch Mac main49.930秒/lib52.901秒编译0，实际8短父例及完整Cpp1父例全部通过（Cpp118.82秒/整组231.178秒，两门约2秒准确拒绝）。target/p6-mac-final-verification-matrix-v7-20261027.json SHA bad80b4276a9ade25dbe5db24bd30ac67fb234fa67fa82186a6aaba1274291fc串联全部旧scope及失败，新main10d8c02f6a347ca46fbac120b683b6724226a134b9a416064bd1345334afe175；缓存六根只必要校核，未整删或改旧矩阵。Linux round16 source9aa7bb848232286a8864dca7b679ab52892f5cdd0a25e6bb54fafbdc9e6dbb63 main178.181秒/lib254.750秒编译0，相关6短父例实际全过（4.15秒/8.051秒整组），当前完整Cpp后Source按原预算串行，仍需两整父成功及服务器清理/指定最终通过。OSABI新SDK保全部1179regular/217572019B及原.a，仅新增四个指向已核/usr/lib实际发行版库的正常开发链接；不再无必要地重建四个PIC源，实际发布仍禁止SDK运行依赖并验证七功能/全部扩展。

Mac现有完整矩阵 `target/p6-mac-final-verification-matrix-20261027.json`（SHA256 `5af5cc10a645c5d606317fcaae58d544816246ba7c5e3605938aa1b19ff2f242`）记录各批源码/二进制及旧失败，不合并为当前整树全绿。指定会话已核154份文件引用及Source逐条命令摘要相符。Mac六后端实际完整父例：setuptools C、CMake/Ninja C++、Meson/Ninja C、公开MarkupSafe3.0.2、maturin、setuptools-rust；Source新recipe2正例328.595秒，含必要标准库/全部已安装扩展/查询/同锁共享/引用拒删/forget/同一删除op续行至物理完成；缺OpenSSL负例184.893秒只在必要ssl功能校验失败且未发布。二者不替代仍缺的Source冷重建证据，后续修复也需明确新的源码对应关系。

本轮新增代码尚未整体验收：独立NativeBuildSession（Linux PID namespace、Mac SID会话）、精确只读执行池/E-R登记及compiler wrapper、一次冻结工具链/SDK、native recipe2及schema4双codec、Cargo固定输入、6个真实后端父例，以及源码CPython来源/锁/语言库接线。原recipe1驱动字节保留。

2026-10-27接续实测：Mac执行门的真实posix_spawn递归已修正，2个实际gate/wrapper组件父例通过；native model/Cargo固定输入等10个数据父例通过，主程序/lib已编译。Linux真实后端仍未通过：测试umask、缺C++及私有SDK断链问题已分别定位并修正输入；随后发现GCC实际LTO插件在库搜索目录之外，已按编译器报告的确切文件补冻结，待复验，未放宽目录准入。Mac首个setuptools在仅对测试进程排除本地代理后已进入build_wheel，但钩子失败，待取得有界错误输出；CPython实际源码首例16.08秒在configure写/dev/null被拒绝，正在修正native worker的精确设备权限。两项均不计通过。Mac私有源码SDK以现有系统SDK的COW副本加现有OpenSSL/xz静态开发文件准备，未改系统SDK或安装全局软件。组件通过不代替真实后端、源码语言生命周期或P6整体通过。

最新进展：指定会话只读预审提出的7项已实现修复，尚待真实行为和正式复审。Mac主程序/lib已重新编译，工具链5例、完整source pin、source commit、来源切换overlay、运行库策略共9个定向数据父例通过；overlay首失败是测试误用Table显示遗漏子表，改用与生产相同DocumentMut后原完整断言通过，未放松生产。Linux同一批main/lib编译通过，工具链4例及source pin/commit/运行库策略共7例通过，overlay待合入测试修正。

真实reflink产品父例已在腾讯云独立btrfs挂载中通过1例（4秒、0ignored），覆盖recipe3、真实ELF运行、A/B共享与独立副本；使用synthetic受管runtime和系统Python，不扩称受管CPython。此前两次权限拒绝及一次ptrace诊断零例均保留；确定sudo重置umask后，在实际测试入口设077才通过。远端专属reflink-run已清理393315959逻辑字节，已卸载且无host mount泄漏，镜像仍随总job最终清理。证据target/p6-tencent-product-reflink-20261027/summary.json。

真实pure setuptools两平台现均实际通过（各1父例、导入42、missing CC、recipe1/native=None）：Mac日志target/p6-mac-setuptools-pure-normal-import-20261027.log，Linux日志target/p6-tencent-round4-pure-setuptools-20261027.log。此前-I屏蔽布局PYTHONPATH及Mac测试项目/var别名两项夹具错误均已修正，未改生产布局或P2符号链接锁门。

Mac真实setuptools C扩展完整父例已通过，169.88秒/1父例/0ignored：实际编译与导入42、A/B同完整锁共享及暖恢复HTTP0、nonce造成冷重建输出变化时拒绝且保留原锁/选择；日志target/p6-mac-setuptools-native-c-locked-warm-20261027.log。此前B未持原锁属于新项目显式解析，不能把元数据请求当暖恢复失败；已改B复制完整锁并保留严格零HTTP及AB锁字节不变断言。Linux C及其他五后端尚未通过。

随后Mac Meson/Ninja真实父例97.50秒及公开MarkupSafe 3.0.2源码父例97.11秒通过，均实际导入/调用42、A/B同锁共享与暖HTTP0，日志target/p6-mac-backend-{meson,public}-fixed-20261027.log。目前Mac原生后端通过3项；C++已修CMake4.4实际share/cmake目录和Mac探测helper、Rust已修固定TLS配置和真实Maturin WHEEL标签入口，待完整复验。

原始Maturin 1.8.6公开轮采用压缩WHEEL Tag（规范原要求expanded），指定审查turn01a1797f-ea3f-7513-885c-608a27ce27ef接受非专属的有界等价入口兼容：保留原字节/摘要/CRC/RECORD/预算、展开前乘积及跨行累计工作检查、最终全集与文件名严格相等。实际wheel组54个父例通过，含原36MB universal2脚本与压缩/非法/超限回归；旧PyYAML ignored用例因未提供其专属输入失败，整组退出101，不宣称55例全绿。日志target/p6-mac-wheel-compatible-suite-20261027.log。

普通生成程序重复输出实际复现两项问题，已最小修复：producer仅解除本session sealed pool的原输出alias，保持pool只读；watcher改为session HashSet保留所有实际owned历史digest，未知pool仍拒。Mac组件先5过/1个150ms启动前测试刺激失败，修正有界就绪/人工deadline后该父例单独通过；Linux utility overlay整组59.834秒、6父例通过/1专属helper ignored。未改产品、600秒、FD或stack额度。另Linux实际argv0对照2.714秒证明GCC用cc基名会从wrapper目录算错builtin头路径，已仅传实际compiler argv0，未加-I掩缺项。

Linux59d77ff3源码的完整nativeC用例在原480秒operation截止失败：A实际GCC编译、导入42及recipe2/schema4核验已完成，B严格暖恢复未结束，cold未进入；父例480.90秒，整组484.761秒，不计完整通过，不提高截止。正审计同持有lease是否重复完整inventory校验。证据target/p6-tencent-round6-native-setuptools-20261027.log。

源码CPython已确认C编译/执行成功，并越过diff修正后的sed检查；新有界诊断实际指出缺rmdir及Mac sw_vers，按确切helper补齐中，不广放目录。Source发布验证另按指定审查要求加载所有已安装lib-dynload扩展后才观察实际动态库，不能以find_spec冒称可选扩展已加载。完整源码语言生命周期与P6最终通过仍未取得。

源码CPython已进一步实际完成configure、解释器/标准库编译及staged安装/import（241.67秒），发布前因LLVM otool调用未登记otool-classic而拒绝；已按真实同目录程序精确绑定classic，原-L对照0且输出相同，待完整重验。已修MacMake选择层：xcrun固定真实gnumake而非make selector；仅允许官方已校验raw源码内固定install-sh执行，不广放工作目录。Mac新接手执行者按同一target继续此正例及剩3后端；P8/P9未进入。

最新Mac Source新full已在原580秒组内实际完成A安装301.914秒、B严格暖复用15.549秒、7必要功能/所有已安装原生扩展、pydoc与idle、同inode、只读快照、引用拒删、forget与prune-dryrun；完整父例402.073秒因指定uninstall原30秒deadline失败，未计通过。已绑定旧op状态证实仅第二次EX库存核验阶段、尚未撤Ready/删除。P4库存回调逐项重开全store控制面已最小分离为held FD/EX guard，完整store/manifest/root入口出口、删除/WAL/cursor/fsync不变；指定预审cursor41静态接受。整个受影响Mac reclaim组8父例通过/2专属helper ignored（81.98秒），含durable退出、恢复及三类损坏拒绝；实际大Source卸载仍待新full，不以小例替代。

Mac C++完整父例155.482秒通过，覆盖CMake try_run、同锁共享/warm0HTTP、已有语言下确定性包冷重建及changed-CC两种ZIP状态拒绝；目前Mac六原生后端通过4项。Rust已越过真实Cargo与registry编译，发现本机Rust1.97 strip=debuginfo产物LINKEDIT损坏；独立rustc对照1.12秒确认strip=none可实际dlopen，Mac Cargo环境固定此输入且记录portable配置，不改原输入/安全边界。SecureTransport选择经同样禁网/拒etc读的Cargo实际0.014秒对照通过；两个Rust全行为待统一刷新复验。

Linux最新去重已让nativeC原480秒内完成A/B严格共享/双导入42/暖0HTTP/锁字节保持，包冷重放仍触截止，未算nonce拒绝通过。指定cursor42接受下一层最窄内部去重：registry循环和Source session checkpoint持同一SH能力时检查原manifest/保护/目录绑定，完整prepare入口、Completed输出/缓存/发布复核和最终commit保留，无新cache/Token/开关或资源提升。已补2个计数/损坏/取消回归，Mac/Linux统一编译与完整行为复验中。

后续Linux完整C在原480/580内通过（474.37/478.162秒），公开MarkupSafe完整364.25秒及Meson完整363.29秒通过，Linux六native已有3项完整通过；C++实际重建指纹已精确相等，冷交接末尾因deadline被旧BuildMismatch包装而失败，未称产物不确定。指定cursor46接受紧邻handoff两处最窄复验去重，最终公共输出/into_parts完整检查保持；已补原cause及回归。LinuxRust版本LLD首行括号内绝对路径漏归一已修入口，validator不放宽；新epoch正编译/执行剩3。

Source旧含自身回归套件配方完整组577.011秒被原controller硬截止（0父例过）：同一公开op实际恢复到6487/8322，未物理完成，原日志/失败状态保留。原30秒和组580/600均未增加。指定审查cursor48明确P6只承诺运行时、未承诺CPython自身回归套件，接受正式默认新版本化配方使用官方`--disable-test-modules`，普通stdlib与unittest/mock/doctest/七必要功能均须保留并实测，查询展示配方事实，旧记录不重新解释。此为产物配置调整，不冒称透明perf或旧8322对象回收通过。新配方代码/新旧记录兼容及正负完整行为由MacAgent实施复验，P6仍未通过。

实际验证：腾讯云utility冷编译4分28秒成功，初始worker exec拒绝；对照确认Linux需对真实ELF PT_INTERP也授Execute，已按精确路径修复，无目录放权。复验`target/p6-tencent-util-recheck-20261026.log`：5父例通过、1专属helper ignored，46.88秒整组；Mac初组3过/1网络测试断言失败，socket创建并不等于联网，改成有正常监听者的实际TCP/Unix连接拒绝后该父例复验通过（`target/p6-mac-native-isolation-recheck-20261026.log`）。这仅utility component，不计6个真实包后端或P6通过。

测试准备全部位于腾讯云专用目录：Rust1.97/RUSTUP_HOME/CARGO_HOME、仅下载解包的sysdeps、当前source/target、native-inputs-linux、runtime-inputs；无apt install/update或全局profile修改。Linux真实PBS输入为`cpython-3.13.16+20261013-x86_64-unknown-linux-gnu-install_only_stripped.tar.gz`，35099487字节/SHA256 `d0080766b6b74aeb81e1949ef0d9a20e6c17f7b160fb295a69b936ccb17660be`，已同时核对GitHub API digest与SHA256SUMS。Mac固定输入在`target/p6-native-inputs-macos-20261026-v2`；v1 Cargo workspace误归属失败日志保留，两Rust模板已显式空workspace，v2准备完成。服务器专用目录结束必须全部清理，不得留下编译进程或SDK/缓存。

为覆盖原先未验的reflink，利用服务器已有btrfs内核/工具，在专用目录创建512MiB稀疏镜像，以sudo的独立mount namespace临时挂载（不改分区/宿主挂载）；kernel FICLONE真实成功、不同inode/nlink1、改副本不改原输入，原始探针`target/p6-tencent-real-reflink-probe-20261026.json`。已退出namespace卸载；镜像仍在专用目录供后续实际Pakro layout用例，最终随目录清理。当前只计文件系统原语，不计产品reflink通过。

- P6.1 Linux独立复制投影与P6.6有界ZIP/raw XZ归档本批实现已完成局部验证和指定审查，阶段范围仍未全部通过。保持原mac recipe1/2兼容、仅新增实际投影方法配方；原hook.py/recipe1请求ID字节语义不改。新增编译协议必须独立版本化并进入两侧codec/预算/缓存同门，不能把路径当portable输出身份。
- 监督核对发现本机Darwin27 SDK与Apple公开XNU均明确NOTE_TRACK/TRACKERR/CHILD不支持（自10.5），filt_procattach返回ENOTSUP；START_SUSPENDED只暂停直接spawn，不能补后续fork归属。现有bounded执行只有process-group/Drop killpg，无父SIGKILL可信后代清理。因此仍保持no-fork，不开放native配方。官方依据：[event.h](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/event.h)、[kern_event.c](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/kern_event.c)。macOS27 `es_new_descendants_client` 实际探针返回 `ERR_NOT_ENTITLED`（3）；当前主程序及探针均无ES entitlement。探针未订阅事件、未派生子进程、未修改签名或安装服务；179秒完整原始证据及摘要见 `target/p6-es-descendants-probe-20261026-030020/summary.json`，探针退出0只表示检查完成，能力不可用。
- Agent已启动原先停止的Docker Desktop，仅创建本轮带所有权label的隔离容器，不改Docker镜像代理或用户容器。Docker Hub实际访问超时后，改用官方Alpine 3.24.3 arm64 minirootfs；SHA256/SHA512双核对输入和导入镜像记录在 `target/p6-linux-inputs/verification.json`。Linux 7.0.14隔离环境仅用于Linux验证，不能代替macOS原生监督证据。P6仍待可信监督、编译配方/解释器构建和实际平台证据，P8/P9未启动。
- 指定“架构设计与代码审查”会话已独立确认当前监督缺口是P6合同阻塞（审查turn `01a1747d-1f1c-73c0-8127-945a8a470985`，完整结论 `target/p6-designated-architecture-blocker-20261026.json`）：ES授权本身也不足以保证监督者死亡后的全部后代清理；目前没有确认到无需额外Apple授权、同时不降低合同的本机方案，但不能据此声称macOS绝对不可能或ES是唯一方案。P6.2及依赖它的P6.3/P6.4/P6.5保持阻塞；P6.1与P6.6可独立继续验收，P6整体不通过，P8/P9不进入。
- 后续可信Darwin `PT_TRACE_ME`直接子探针通过normal、owner SIGKILL及注册失败注入三种情况（167秒，`target/p6-darwin-traced-child-probe-20261026/summary.json`）。这只是叶子生命周期候选：普通fork不继承P_LTRACED，exec/SIP/hardened及抗调试未验证；所有工具no-fork时，Python subprocess代理也不能承接Ninja/CMake/Cargo/rustc内部原生spawn/wait。缺少版本化的原生工具执行适配及完整归属证据，不能据此开放既定后端；分析与官方源码位置见 `target/p6-native-broker-assessment-20261026.json`。
- P6.1/P6.6本批生产改动：原mac recipe1/2保持；新增Linux实际reflink recipe3/独立核验复制recipe4，只有明确能力错误才受控丢弃整个未发布布局并全图复制；增加有界ZIP sdist的索引/锁/解包，raw TAR.XZ独立入口使用64MiB硬解码内存上限，未授予语言发布能力，原hook.py/recipe1请求身份不变。
- 本批源码候选 `target/p6-final-source-20261026.json` SHA256 `6171fbf73d89eb313160330e209be08b7970694a0669b3512309ca63246e7afa`。Mac行为对应前一快照 `b4a05b8dd6b7142fadba7a8d654fcb5ba0996b6c9bee23e37868eea218114391`，与最终快照仅差Linux专属测试两处参数借用；Mac组前后源码一致，Linux实际容器源码核对最终快照无差异，不能混称一次全套绿灯。
- Mac定向组 `target/p6-mac-final-unit-20261026.log`：22通过、0失败、1ignored（423.94秒行为，加本次编译仍小于600秒），含旧投影、原TAR及新增ZIP递归输入/严格冷暖/暖零HTTP与零build/nonce实际输出变化拒绝。ignored的官方CPython 3.13.16 raw用例已另行明确执行，`target/p6-real-cpython-raw-20261026.log` 1通过/0ignored，3.02秒；索引组 `target/p6-index-unit-20261026.log` 只有1个实际库用例计通过，零例的集成binary不计通过。
- Linux原始组 `target/p6-linux-unit-libclang-20261026.log`：编译3分31秒，14通过/2夹具失败；缺libclang的首次构建、/usr/bin/sed脚本夹具失败及新增Linux测试借用编译错误均保留原日志。修正只涉及测试夹具，不放松生产musl ELF/loader检查。最终 `target/p6-linux-layout-borrow-fix-20261026.log` 3通过/1ignored，实际方法VerifiedCopy；另显式运行ignored真实ELF单例 `target/p6-linux-elf-layout-20261026.log` 1通过/0ignored，核邻接依赖拆分失败、真实bootstrap导入/ctypes返回42、两项目catalog共用布局及独立inode。实际解释器为容器系统Python 3.14.8，受管runtime为明确的协议ELF夹具，只证明Linux musl arm64布局，不能算源码扩展或受管Python验收。
- Linux官方CPython raw归档 `target/p6-linux-real-cpython-raw-20261026.log` 1通过/0ignored（2.68秒），索引 `target/p6-linux-index-20261026.log` 1通过。官方原归档固定23225704字节/SHA256 `f4b1bfb3c79b5bb11b8d228a12504163b4c0dab4d679828d8f5f26b6cb6ab35d`，两平台均经实际入口解包5272项并核关键源码/私有清理，未运行configure或发布语言。真实reflink成功路径、其他Linux文件系统/架构及完整P6编译后端仍未覆盖，不推断通过。
- 为消除Docker默认高句柄上限的证据缺口，最终在同一Linux源码/实际binary上把nofile软硬上限均降至256、未设RUST_MIN_STACK，以完整输入显式include ignored执行全部当前P6定向父例：`target/p6-linux-final-bounded-20261026.log` **18通过/0失败/0ignored，3.18秒**。这只是该Linux配置的P6定向组通过，不是全工程或P6阶段通过。主程序及CLI测试分别47.38秒、43.65秒编译成功，日志/各轮失败/源码对应及产物SHA归集 `target/p6-scoped-verification-20261026.json`。
- Mac也将nofile软硬上限降至256，单独复验复制/旧新投影/归档及真实raw输入，`target/p6-mac-final-bounded-20261026.log` 21通过/0ignored，8.48秒；已通过的两个长冷暖用例未重复执行，也不把本次限额覆盖扩大到它们。另按用户要求核对并删除7个不含现行必需直接xz2依赖的过期根库编译文件，释放675152403字节，保留两份当前依赖图、全部binary/有效输入/原始证据；逐文件SHA/uid/inode/模式/时间及指纹核对见 `target/p6-obsolete-build-cleanup-20261026.json`。
- Linux必要的3份可信C输入已按原字节保存到 `tests/support/fixtures/p6_linux/`，准备脚本 `tests/scripts/p6-linux-fixtures.py` 仅串行编译这3个输入到全新私有目录，整组120秒，不安装工具/不运行产品worker/不覆盖现有目录。实际准备0.125秒成功，3产物SHA均复现原输入；现有目录拒绝退出1且18个原条目不变、未执行编译器；消费该脚本JSON/env的真实ELF单例再次通过。日志见 `target/p6-linux-portable-fixture-{prep,refusal,elf}-20261026.*`。准备命令形态为 `python3 tests/scripts/p6-linux-fixtures.py <不存在的绝对目录> --cc <已安装cc> --expect-projection-method verified-copy|linux-reflink`；方法参数只是调用者的测试断言，不猜测TempDir文件系统。
- 本批提交局部审查的最终快照为 `target/p6-review-source-20261026.json` SHA256 `13cc1852e87e4053731908caf257fb2bc39074f3740769e6cd0691e209a8943e`：较已编译核心6171只新增上述4个测试支持文件，Rust/Cargo/生产字节未变；Host前后快照及Linux实际3119项源码核对一致。Darwin候选探针按审查建议另存v2，仅加SIGKILL退出事件断言并只复验kill模式，22秒通过，未覆盖v1；`target/p6-darwin-traced-child-probe-20261026/summary-v2.json`。仍不代表P6.2或阶段通过。
- 指定审查途中因压缩上下文误用旧Git授权而生成的本地提交已纠正：HEAD/索引恢复到原 `3acf1a9912d02cd0eda1f257026ce747090ef840`，3123项工作文件内容/模式/mtime完全保留，源码快照仍为13cc；原提交 `9368d8e55` 可由ORIG_HEAD/reflog恢复，未删除Git对象。审计 `target/p6-unrequested-commit-recovery-20261026.json`。本轮仍无Git提交/推送授权；Git提交结果不作为局部审查结论。
- 指定会话随后完成实际局部审查，并纠正旧Git授权误判。审查turn `01a174a4-ff59-7692-88cf-d71a2883392c`、复核/更正turn `01a174a8-8801-7d52-a95b-33e298645bbd` 明确接受本批独立复制布局/ZIP/冷暖/raw XZ实现，未发现确定阻塞，核对13cc全部3119项和验证摘要一致；原结论及更正见 `target/p6-designated-limited-review{,-details}-20261026.json`。明确不把真实reflink、受管Linux CPython、原生扩展或P6阶段算作通过，不能跳P8/P9。
- 审查完成后，保留全部日志、源码/输入摘要及小型真实ELF证据，销毁仅本轮所有权label/实际ID绑定的Docker容器和精确镜像tag，清除6303379456字节容器可写层；未操作其他容器/镜像/volume，清理后各类计数均为0。审计 `target/p6-owned-docker-cleanup-20261026.json`，可信夹具小归档 `target/p6-linux-portable-products-20261026.tar`。Docker停止命令退出0，后续daemon socket不存在且backend/UI进程无匹配，已恢复原停止状态；单次Desktop状态查询10秒超时不计为成功状态响应。不把容器逻辑清理字节等同宿主稀疏磁盘即时缩减。

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

- 安装脚本只能在本次拥有的私有准备布局中运行。
- 禁止联网、修改其他项目、共享原件或未授权宿主位置。
- 脚本新增和修改的文件进入所有权与结果核验。
- 激活脚本在准备阶段受控求取环境变化，保存为可应用的环境描述。
- 进入项目目录应用已有环境描述；离开时用现有环境差分恢复。
- 不在每次进入目录时重新执行任意包脚本。
- 依赖交互、秘密状态、外部写入或无法表示的动态行为明确拒绝。
- 不允许脚本调用Conda/Mamba等管理器补装依赖。

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
| 提交 | 准备失败不切换；并发和外部编辑不会覆盖他人修改；中断可恢复。 |
| 删除 | 仍被引用或使用时不删；无人使用的对象、布局及依赖环可显式回收。 |
| 构建 | 实际编译结果可运行，工具及输入明确，无隐藏依赖下载或管理器委托。 |
| Conda | 配套解释器唯一，混合冲突明确，脚本受控，冷暖恢复与回收闭合。 |
| 只读 | 查询、预览及被拒绝操作不写业务状态、不补装、不自动恢复。 |
| 资源 | 无泄漏、无限重试或绕过预算；失败状态如实输出。 |

### 验证纪律

- 只运行本批变更和受影响路径需要的测试，不每次重跑全部历史测试。
- 每个宣称完成的能力必须有实际行为证据；零测试、全ignored、只编译成功均不算功能通过。
- 长构建与长验证继续由用户终端执行；自动多进程运行器保持禁用。
- 同一批使用既定target、features和构建配置，遵守每组600秒规则。
- 日志放`target/`，记录命令、输入、源码对应关系、实际退出码、用例数和未覆盖项。
- 不能以增加无关测试数量替代关键失败场景。

**本轮范围止于P6。** P3、P4、P6的保留范围已全部完成并有实际证据；P8（Conda直接管理）与P9（Python收尾与完整平台矩阵）用户已明确不在本轮实施，不构成当前完成条件。已取消内容不再成为完成条件；缺少平台或输入证据时保持待验，不推断通过。

## 五、实施Agent与审查Agent的交接规则

### 实施Agent

- 按依赖顺序实施上述工作，不恢复已取消功能。
- 复用现有生产内核，删除被替代且没有消费者的旧实现。
- 每完成一批，在PLAN中记录：完成内容、实际验证、尚未解决的问题。
- 提交审查时提供对应代码差异、实际命令、完整日志位置和真实结果。
- 遇到无法满足合同的技术问题，应报告具体阻塞，不自行降低保护、偷偷换来源或扩大产品范围。
- 不新建HANDOFF、TESTS、FILES、开发日志或平行计划；不自动提交或推送Git。

### 本会话负责审查

- 只读核对代码差异、实际调用链和原始验证证据，不把实施Agent的总结直接当作结论。
- 重点检查范围是否扩大、旧路径是否回流、共享与引用是否正确、锁是否被静默改写，以及失败和恢复是否真实成立。
- 明确区分阻塞问题、普通建议和未覆盖证据。
- 修复交回实施Agent；审查依据发生变化后，只复核受影响部分。
- 完成结论必须绑定实际源码和验证结果，不以文档勾选、测试名称存在或旧阶段通过代替。

全过程继续停用dev-flow，不恢复其固定TDD循环或审查流程。
