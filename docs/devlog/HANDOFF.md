# F1–F4 完成交接 — 2026-10-20 19:19（Asia/Shanghai）

## 当前结果

F1–F4已按2026-10-20用户授权完成本机macOS arm64首版实现、Agent自测与Spec/Standards审查。用户已明确要求提交本轮改动；本次提交仅包含源码、测试源码和开发记录，提交号以git log -1为准，未授权push。工作树最初干净于70f7fbfc7，不reset/stash。原架构D07–D10及PLAN首版边界不变，不自动开始后移任务。

- F1：目录按真实有效配置、完整版本/来源pin选择已有Python或包布局；缺项只提示，command-not-found/wrapper不隐式补装；成功选择通过同catalog维护关联。
- F2：显式完整版本/build安装；真实来源/摘要与已有锁pin一致；同版本多build拒绝自动选择；旧force/repair/postinstall/uninstall/purge/rollback/system publication等无许可旁路在副作用前拒绝。
- F3：原solver/wheel/来源证据核形成真实闭包；包树共享、轻量启动适配，import/console/-m/.pth/原生扩展与循环依赖可用；同权威catalog保存完整精确图及项目选择，不写项目声明或锁、不委托pip/uv。
- F4：真实内容/当前选择/项目/引用链查询；指定语言/包instance受真实EX和最新引用检查保护；损坏/丢失权威不能当空、活租约/引用拒绝、部分删除撤销Ready；纯Python查询避开旧Tracker写路径。

## 验证证据

64个不同核心用例全部有GREEN：F1 12、F2 19、F3 21、F4 12。`target/f-core-final-20261020.log`44/44（F1/F2/F4及F3目录，53.6秒）；`f3-accepted-20261020.log`20/21，唯一旧目录fixture使用任意graph常量被强校验拒绝，改为真实fingerprint后已在上述44项GREEN。生产校验未放宽。3条ignored staging助手不计功能数。

底层/相邻回归：`f-store-final-20261020.log`54/54；`f-final-model-prefix-pipeline-regression-20261020.log`62/62；`f-final-codec-regression-20261020.log`12/12；`f2-native-catalog-regression-20261020.log`26/26。完整性4项均真实RED→GREEN：锁entry build矛盾、图roots/edge摘要、已提交catalog丢失、C4完成后同inode正文篡改。各批存在重叠，不相加冒称全部不同测试。

八条CLI全部通过：

| 用例 | target日志 | 秒 |
| --- | --- | --- |
| F1目录选择 | f1-cli-accepted-20261020.log | 10.7 |
| F1命令查找零补装 | f1-cli-command-accepted-20261020.log | 60.9 |
| F2官方真实下载/运行/双项目共享/重装复用 | f2-cli-socks-accepted-20261020.log | 300.9 |
| F3共享wheel/脚本/.pth/组件复用 | f3-cli-shared-accepted-20261020.log | 88.1 |
| F3 deps不改声明/锁 | f3-cli-deps-preserve-accepted-20261020.log | 121.1 |
| F3不调用外部manager | f3-cli-no-manager-accepted-20261020.log | 120.3 |
| F4查询/引用拒删/明确instance实际删除 | f4-cli-accepted-20261020.log | 53.9 |
| F4纯Python查询不初始化/登记/写元数据 | f4-query-cli-green-20261020.log | 7.8 |

额外实际公开PyPI `idna==3.10` 安装、中文域名编码及项目/包查询通过64.5秒（`target/f3-live-pypi-20261020.log`）。包真实联网获取，未做本地Simple替换；运行时复用已授权原PBS通过原内核暂存。

主CLI编译 `f-cli-final-build-20261020.log`通过72.9秒；测试编译 `f-final-lib-build-20261020.log`通过69.6秒；注册检查 `f-final-registered-20261020.log`通过；格式 `f-final-format-green-20261020.log`通过，git diff--check、全部变更shell语法及fixture Python AST均通过。最后仅query/mod.rs声明排序，无行为变更，未为此重复重编。原有dead_code/linker/future warnings保留，不代表全仓无warning。

## 网络与边界

HTTP代理正式组曾在catalog send失败；独立同native client诊断捕获macOS native TLS -9806连接关闭（timeout=false），随后同配置200。一次协议对照官方tag两协议均200，checksum HTTP握手失败、SOCKS200；不据此声称所有历史失败同因或长期网络稳定。完整官方CLI通过机器已有SOCKS接入，未改来源、版本、摘要、期限或加入生产重试策略。需复跑该机器网络用例时可用：

```bash
cd '/Users/fuxinao/文件/project/pakro'
HTTPS_PROXY=socks5h://127.0.0.1:7892 HTTP_PROXY=socks5h://127.0.0.1:7892 tests/e2e/run_test tests/e2e/cli/test_f2_exact_python_install
```

仅验本机macOS arm64、官方PBS CPython、单公开Simple兼容wheel首版。Linux/PyPy完整矩阵、源码/URL/Git/editable/Conda、多私有源、完整锁改写/恢复、prune及外部进程全生命周期按PLAN后移；现有wheel核的ASCII数据根约束保留。此首版完成不等于长期架构全部完成。

## 执行与存储

本轮全部命令组少于600秒，未触发用户要求的10分钟停机交接。Agent未来仍遵守用户最新授权和600秒墙钟上限；用独立ASCII TMPDIR和进程组控制，只停止自己启动的进程，不拆分绕过长测试。统一构建配置：

```bash
export CARGO_TARGET_DIR='/Users/fuxinao/文件/project/pakro/target'
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
```

`tests/scripts/f-manual-check.sh`支持f1–f4、--list、--format-only与第二参数--lib-only；F4另外运行pakro-store用例，不混入pakro数量。Agent本轮以 `/usr/bin/python3 /tmp/pakro-f-bounded-20261020.py LOG COMMAND...`加600秒上限；不要使用可能被用户shim改写的裸python3运行该编排器。

最终target约6.0GiB；已精确清重复profile4426.7MiB、旧测试临时库356.8MiB（合计约4.67GiB），另清确认归属的诊断可执行约36.96MiB。报告在target/*cleanup-20261020.json及f2-diagnostic-cleanup-20261020.log。所有本轮bounded-check临时根剩余0，未留测试/编译进程。保留可用main/test缓存和日志，没有清用户库。

原输入保留：`/Users/fuxinao/文件/project/pakro_test/cpython-3.13.16+20261013-aarch64-apple-darwin-install_only_stripped.tar.gz`，25268458字节，SHA256 `16a5332fb0dcb77f163fc8f8cbccd95be1644cc53e68e5d2a79f50afd6680a0e`。

主日志 `log/2026-10-20-1-F1至F4接续.md`；包库/依赖图/F4各有当日日志。当前没有首版待完成事项，后续按用户新指令安排。
