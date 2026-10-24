# 本轮进度截止：ISAT已在Pakro环境成功运行 — 2026-10-24

## 截止与下一步

用户按force hook、sys.executable guard及python main.py命令操作后确认“成功运行了”，明确要求提交Git并在此截止。真实项目完整依赖安装、环境选择和ISAT基础启动的人工验收已通过；本轮不再继续性能调查、自动冷组或其他生态开发。后续任务等待用户新的明确指令。

本轮提交纳入原F1–F4提交77d5969a8之后的相关源码、测试、开发记录，用户已授权本地Git提交，不push。当前提交号以git log -1为准。真实ISAT目录不在本Git仓库内，不擅自git init或另建仓库。

## 实际用户环境

- 项目：`/Users/fuxinao/Projects/ISAT`；pakro.toml已按原mise固定版本补齐24项直接依赖，Python为`3.11.17+20261013`。原源码、mise配置及模型保留。
- 登记identity：`a4044bf3-f9a8-5f6d-ac3a-b13ccd73ba24`，revision3，关联60个包。
- 共享语言：`language-670a04e9cf8e3636a5fd4a9fcfcd15e2d2153f1b1acb7be6e0ebd66e9018244e`。用户此前已恢复实际4个发行pyc并保持其余2240项，不重复要求恢复。
- 受控恢复仍仅限原官方归档SHA `d81d757a9785850393cd695a9a686c765a6370fbdc83738589ef2ecd3e5c44b3` 与该实例，不开放通用repair/force。正常启动保持PYTHONDONTWRITEBYTECODE。

## 已完成实现与证据

- registry纯Python.tar.gz的PEP517/legacy构建：初始/动态build requires递归、真实worker隔离、同solver续传、原源码与实际wheel双证据、共享发布与同catalog提交；不委托pip/uv。实际iopath0.1.10、Hydra1.3.7/ANTLR4.9.3核心安装/使用/跨项目复用173.9秒通过。
- 来源代理/期限、PyYAML多Generator兼容、Simple顶层versions与文件facts兼容、候选成本及回溯修复；resolver72、candidate_policy20、registry55相关147项通过。多项目排序建议仅同一输入快照内保留，完整候选、nogood、预算与每轮策略保留。
- 原生相邻加载COW布局；逐包重复语言全量检查收束为纯物化批次前后，独立入口和发布前/后保护保留。
- EMFILE现场修复：用户soft256/hard unlimited，Pakro仅把自身soft准备至min(hard,4096)，hard/父shell不变，高soft不降；64真包完整布局、canonical获取和Drop后FD恢复及真正CLI继承均通过。不是17N FD池优化或硬256图支持。
- 环境选择现场修复：包full walk用私有held FD视图、前后完整store/manifest核对，组内pending捕获后一次full再交出lease；独立获取及跨commit重获保留。选择入口一次冻结300秒协作式总期限，原offline/write/guard与CLI快路保留，无跨操作hash缓存。
- 最后相关组：包库44通过/1辅助ignored、directory3通过，main all-features成功，整组409.5秒；真实只读环境选择32.71秒通过，之后用户实际启动成功。未重跑未变化的长测试。Cargo发行清单漏hook.py已实际list RED→GREEN补齐，未改变已启动成功的业务逻辑。

## 明确未覆盖与后续边界

- 先前完整自动冷组达到600秒已停，仍未完成；不能回写为自动全套通过。
- 用户“成功运行”只确认基础启动，不证明模型推理、NMS/标注格式导出全套自动断言、完整ISAT图重复/跨项目零获取均已通过。S4按手动安装/选择/启动目标收尾，这些单列为未覆盖项。
- worker仍denyfork，C/C++/Rust编译器源码后端未开放；本次ISAT两个必要源码包为纯Python，其余使用匹配平台wheel。
- 完整选择仍约33秒；FD锚/重复持有进一步优化、下载性能后移，不自动继续。验收driver有本机测试binary哈希/归档/ISAT/代理路径，不能称跨机器直接可跑。

## 执行与存储

所有Agent执行组仍≤600秒，超时停止并联系用户；单Cargo、同target、CARGO_INCREMENTAL=0、DEV/TEST_DEBUG=0。完成组owned临时树已清理，无后台Agent安装/编译/下载。target、有效原发行输入、用户库和临时测试输出不入Git；保留有效main/测试缓存，incremental为空，不清用户数据。

恢复读取：本文件 → PLAN S当前截止 → log/2026-10-24-4-ISAT验收截止与提交.md；详细修复见同日1/2/3日志，前轮字节码/来源/S1–S3见2026-10-20至23按时间追加日志。历史log/ADR只追加不改写。
