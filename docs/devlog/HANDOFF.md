# 当前交接：P1本机macOS arm64产品验收通过，收尾提交 — 2026-10-25

## 用户最新裁定

**P1 本机 macOS arm64 产品验收通过，自动多进程运行器禁用。** 用户已核对当前代码和原始手动日志，未发现新的产品阻塞；自动运行器启动前明确拒绝，不再将通用监督器追加为P1产品验收门槛。后续长测试继续由用户终端执行。

用户授权将既有P1实现、测试及收尾文档提交Git，本轮不改任何代码。P2尚未开始；跨平台、完整ISAT业务/自动冷组、其他生态和后续阶段边界继续保留。整个项目停用dev-flow。

## 验收依据

- 原始日志：`target/p1-recheck-manual-all-20261025-002758.log:4551`主程序构建成功1分07秒；扩展准入E2E7秒通过11次拒绝快照/HTTP断言与NonPy正例；同名优先级E2E231秒通过env/hook/which一致、显式定向与缺Py companion；最终all exit0。
- 此前精确Python冷安装CLI正例、A14同配置前后资源对照，以及相关F/S/只读/并发/恢复证据仍有效，详见TESTS。A14限定选择/应用内核，不能扩大为完整ISAT性能或跨平台验收。
- 自动多进程tree模式仍在启动前返回125、coverage_complete=false、survivors=null；明确single-process模式的短实验独立保留，不能据此宣称通用树监督可用。这个工具能力边界不再阻塞P1产品验收。

## 提交与接续

本次提交包含从基线`3d2a6c291`以来的P1源码、测试、运行脚本、AGENTS与文档。target、原始log、归档、wheel、用户库和测试临时数据不入Git。本轮更新前后校验全部非Markdown文件内容及mode不变，未重跑构建/长测试，复用既有手动证据。

提交完成后的HEAD即本次P1收尾提交；具体hash见Git历史。后续工作按用户新指令从P2或其他明确范围进入，不自动开发下一阶段，也不恢复旧R任务/历史授权。

以下保留此前实现与验收追溯；其中旧“未提交/不能关闭”等措辞只表示当时状态，以本节用户最新裁定为准。

## P1交付

- 公共`query_python(PythonQueryRequest)`统一语言与完整环境事实，`apply_directory_python()`独立关联；显式目标、前缀、scope、别名及错误保真，where语言根与环境路径职责分开。
- Config只读原始文件receipt桥接旧Config与新观察，拒绝文件改写/新增来源；目录应用包括already-applied快路均复核当前性，保留首次300秒查询上界及原锁序。
- 真实F/S继续走`programs/python.rs`和native/runtime/solver/build核；重复PBS-only apply正文退出，断言迁到实际核。保持runtime pin、完整根、源码/输出双证据、native COW及严格Locked包拒绝。
- 旧Python写、间接依赖补装、force/postinstall及全部install-into在真实入口提前拒绝；读取启动无维护、Tracker、信任持久化、插件setup、env-cache或TTY hint写。明确非Python的原查询与shim/wrapper/lazy/not-found已验收。
- 精确4-pyc恢复仍只限既定例外；测试均在owned隔离根，没有改用户库、ISAT或真实4项。

## 验收与记录

详见[TESTS的P1 A01–A15和日志索引](TESTS.md)、[FILES实际消费者/接缝](FILES.md)、[本轮日志](log/2026-10-24-6-P1实施.md)。

已通过：P1核心17＋目录3＋安装2；追加同项目并发竞争1；CLI分类2；5条P1边界E2E；真实shared完整查询/hook；原非Python4条查询；F1–F4核心104项（4个ignored不计）；S核心33；真实iopath/Hydra/ANTLR构建/使用/重复及跨项目；恢复4项/2240项不变；F1命令及F4删除/只读3条E2E；最终完整性/公开合同/恢复分类19项。组之间有重叠，不求和冒称不同用例总数。

资源JSON在`target/p1-resource-baseline.json`：64真实wheel、448文件/76608字节，冷/重复进程来源连接0，FD 11→1227/1228→11；每query包full inventory128、首次commit192、已关联commit0。runtime是确定性fixture，不是ISAT性能数据。原64包soft256/hard4096与释放、父进程/hard不变及高soft不下降均通过；未改300秒/4096或完整hash/namespace检查。

已核对18项P1库注册及CLI/E2E清单，Rust格式、脚本语法和diff检查通过；已清理9个已诊断的本轮owned失败根，无本轮后台安装/编译/下载。

每组均低于600秒，最长已运行组380.9秒（早期runner失败）；最长通过组366.5秒。早期编译、夹具、runner问题均保留失败日志并已修复，不回写历史为通过。构建保持同target及`CARGO_INCREMENTAL=0`、`DEV/TEST_DEBUG=0`，有既存dead_code/链接器/依赖future-incompat警告。

## 保留边界

完整ISAT自动冷组、完整24根图复用/业务/NMS/模型推理、Linux及其他架构/PyPy、外部进程完整生命周期仍未测或后置。当前P1不是Python完成门。统一锁恢复、声明增删升级事务、批量prune、完整来源/编译器/editable/Conda未开放；动态执行配置、未知metadata及不支持的Python查询组合明确诊断。

恢复读取：本文件 → 根主架构当前状态 → PLAN前部P1结果与P2 → TESTS/FILES。前轮ISAT人工截止事实如下，保留其原始含义。

## 前轮截止记录：ISAT已在Pakro环境成功运行

### 截止与下一步

用户按force hook、sys.executable guard及python main.py命令操作后确认“成功运行了”，明确要求提交Git并在此截止。真实项目完整依赖安装、环境选择和ISAT基础启动的人工验收已通过；本轮不再继续性能调查、自动冷组或其他生态开发。后续任务等待用户新的明确指令。

前轮提交3d2a6c291纳入原F1–F4提交77d5969a8之后的相关源码、测试、开发记录，用户授权的该次本地提交已经完成，未push。真实ISAT目录不在本Git仓库内，不擅自git init或另建仓库。

### 实际用户环境

- 项目：`/Users/fuxinao/Projects/ISAT`；pakro.toml已按原mise固定版本补齐24项直接依赖，Python为`3.11.17+20261013`。原源码、mise配置及模型保留。
- 登记identity：`a4044bf3-f9a8-5f6d-ac3a-b13ccd73ba24`，revision3，关联60个包。
- 共享语言：`language-670a04e9cf8e3636a5fd4a9fcfcd15e2d2153f1b1acb7be6e0ebd66e9018244e`。用户此前已恢复实际4个发行pyc并保持其余2240项，不重复要求恢复。
- 受控恢复仍仅限原官方归档SHA `d81d757a9785850393cd695a9a686c765a6370fbdc83738589ef2ecd3e5c44b3` 与该实例，不开放通用repair/force。正常启动保持PYTHONDONTWRITEBYTECODE。

### 已完成实现与证据

- registry纯Python.tar.gz的PEP517/legacy构建：初始/动态build requires递归、真实worker隔离、同solver续传、原源码与实际wheel双证据、共享发布与同catalog提交；不委托pip/uv。实际iopath0.1.10、Hydra1.3.7/ANTLR4.9.3核心安装/使用/跨项目复用173.9秒通过。
- 来源代理/期限、PyYAML多Generator兼容、Simple顶层versions与文件facts兼容、候选成本及回溯修复；resolver72、candidate_policy20、registry55相关147项通过。多项目排序建议仅同一输入快照内保留，完整候选、nogood、预算与每轮策略保留。
- 原生相邻加载COW布局；逐包重复语言全量检查收束为纯物化批次前后，独立入口和发布前/后保护保留。
- EMFILE现场修复：用户soft256/hard unlimited，Pakro仅把自身soft准备至min(hard,4096)，hard/父shell不变，高soft不降；64真包完整布局、canonical获取和Drop后FD恢复及真正CLI继承均通过。不是17N FD池优化或硬256图支持。
- 环境选择现场修复：包full walk用私有held FD视图、前后完整store/manifest核对，组内pending捕获后一次full再交出lease；独立获取及跨commit重获保留。选择入口一次冻结300秒协作式总期限，原offline/write/guard与CLI快路保留，无跨操作hash缓存。
- 最后相关组：包库44通过/1辅助ignored、directory3通过，main all-features成功，整组409.5秒；真实只读环境选择32.71秒通过，之后用户实际启动成功。未重跑未变化的长测试。Cargo发行清单漏hook.py已实际list RED→GREEN补齐，未改变已启动成功的业务逻辑。

### 明确未覆盖与后续边界

- 先前完整自动冷组达到600秒已停，仍未完成；不能回写为自动全套通过。
- 用户“成功运行”只确认基础启动，不证明模型推理、NMS/标注格式导出全套自动断言、完整ISAT图重复/跨项目零获取均已通过。S4按手动安装/选择/启动目标收尾，这些单列为未覆盖项。
- worker仍denyfork，C/C++/Rust编译器源码后端未开放；本次ISAT两个必要源码包为纯Python，其余使用匹配平台wheel。
- 完整选择仍约33秒；FD锚/重复持有进一步优化、下载性能后移，不自动继续。验收driver有本机测试binary哈希/归档/ISAT/代理路径，不能称跨机器直接可跑。

### 执行与存储

所有Agent执行组仍≤600秒，超时停止并联系用户；单Cargo、同target、CARGO_INCREMENTAL=0、DEV/TEST_DEBUG=0。完成组owned临时树已清理，无后台Agent安装/编译/下载。target、有效原发行输入、用户库和临时测试输出不入Git；保留有效main/测试缓存，incremental为空，不清用户数据。

恢复读取：本文件 → 根主架构“已确认的新一轮架构与顺序” → PLAN前部P1–P9/P1。追溯能力时读交叉盘点与PLAN后部历史F/S及log/2026-10-24-4-ISAT验收截止与提交.md；本次文档确认见log/2026-10-24-5-Python架构与P1计划.md。历史日志不改写。
