# PLAN — M0 地基（当前执行范围）+ 里程碑索引

> 输入：SPEC.md（待确认）、ARCHITECTURE.md、ADR-0001..0004、详细设计《mise-包管理架构设计.md》。
> 计划纪律：本文件只详列 **M0** 的可执行任务；M1 起每个里程碑在前一个验收通过后按同一标准细化并追加到本文件（记 log）。这是范围决策，不是留空。
> 仓库纪律提醒：本仓库要求 `cargo clippy --workspace --all-features --all-targets -- -D warnings` 无豁免通过（AGENTS.md），禁止 `#[allow]`；commit 遵循约定式提交，且**仅在用户明确同意后执行 git 提交**。

## M0 文件结构图

```
Cargo.toml                        [改] workspace members += 两行
crates/
  mise-store/
    Cargo.toml                    依赖: serde/serde_json/thiserror/sha2/hex/flate2/tar（dev: tempfile）
    src/lib.rs                    crate 入口: pub mod error/identity/path/store/refcount; 预留 gc 模块位
    src/error.rs                  StoreError + Result 别名
    src/identity.rs               Eco/Platform/RuntimeId/Digest/PackageIdentity + Display/FromStr
    src/path.rs                   entry_dir(root, identity) -> PathBuf
    src/store.rs                  Store::open/entry_path/contains/unpack（digest 校验+tar.gz 解包+幂等）
    src/refcount.rs               refs 读写(add_ref/remove_ref/refs) + gc(&Store) -> GcReport
  mise-resolver/
    Cargo.toml                    依赖: petgraph/serde/thiserror（dev: 无）
    src/lib.rs                    pub mod graph/error
    src/error.rs                  ResolverError（Cycle/MissingNode）
    src/graph.rs                  DependencyRequest/ResolvedNode/ResolvedGraph/sorted()/install_order()
tests/（各 crate 内 tests/ 目录, 集成测试）
```

关键签名（全计划统一以此为准）：

```rust
// mise-store
pub fn entry_dir(root: &std::path::Path, id: &PackageIdentity) -> std::path::PathBuf
pub struct Store { pub root: std::path::PathBuf }
impl Store {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self>;
    pub fn entry_path(&self, id: &PackageIdentity) -> PathBuf;
    pub fn contains(&self, id: &PackageIdentity) -> bool;
    pub fn unpack(&self, id: &PackageIdentity, bytes: &[u8]) -> Result<PathBuf>;
    pub fn add_ref(&self, id: &PackageIdentity, project_root: &str) -> Result<()>;
    pub fn remove_ref(&self, id: &PackageIdentity, project_root: &str) -> Result<()>;
    pub fn refs(&self, id: &PackageIdentity) -> Result<Refs>;
}
// mise-resolver
impl ResolvedGraph {
    pub fn sorted(&self) -> ResolvedGraph;
    pub fn install_order(&self) -> std::result::Result<Vec<String>, ResolverError>;
}
```

依赖版本对齐规则：新增依赖前先 `grep -A1 'name = "<crate>"' Cargo.lock`，与主 crate 已用版本保持同一主版本，避免编译两份。

---

### Task 1: workspace 挂载与两 crate 骨架

**Files:**

- Modify: `Cargo.toml`（workspace members 列表，1-17 行区域）
- Create: `crates/mise-store/Cargo.toml`、`crates/mise-store/src/lib.rs`
- Create: `crates/mise-resolver/Cargo.toml`、`crates/mise-resolver/src/lib.rs`

**Interfaces:**

- Produces: 两个可编译的空 crate，后续任务在此之内填模块

**验收标准：** `cargo check -p mise-store -p mise-resolver` 通过；`mise run lint`（hk，改动文件）通过。

- [x] Step 1: 根 `Cargo.toml` members 增加 `"crates/mise-store"`、`"crates/mise-resolver"`（保持字母序插入合适位置）
- [x] Step 2: `crates/mise-store/Cargo.toml`：

```toml
[package]
name = "mise-store"
version = "0.1.0"
edition = "2024"
description = "Content-addressed package store for mise"
license = "MIT"
publish = false

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
sha2 = "0.10"
hex = "0.4"
flate2 = "1"
tar = "0.4"

[dev-dependencies]
tempfile = "3"
```

- [x] Step 3: `crates/mise-store/src/lib.rs`：

```rust
//! Content-addressed package store: identity -> disk layout, dedupe, refcounting.
pub mod error;
pub mod identity;
pub mod path;
pub mod refcount;
pub mod store;

pub use error::{Result, StoreError};
pub use identity::{Arch, Digest, Eco, Libc, Os, PackageIdentity, Platform, RuntimeId};
pub use refcount::gc;
pub use store::Store;
```

同构创建 `crates/mise-resolver`（deps: `petgraph = "0.8"`、`serde = "1"`(derive)、`thiserror = "2"`；lib.rs 先只 `pub mod error; pub mod graph;`——此时文件尚不存在，先建空的 `error.rs`/`graph.rs` 只含模块级文档注释，Task 5 填充）。**例外**：为让 Step 4 的 check 通过，Task 1 里 lib.rs 直接注释掉 pub mod 行，Task 2/5 逐个放开——或者更简单：Task 1 的 lib.rs 只写文档注释无模块声明，后续任务加模块时同步加 `pub mod` 行。采用后者。

- [x] Step 4: `cargo check -p mise-store -p mise-resolver` 通过；`grep` 核对 thiserror/petgraph 与 Cargo.lock 主版本一致（不一致则改用 lock 中版本）
- [x] Step 5: 提交：`chore(packages): add mise-store and mise-resolver crate skeletons`

---

### Task 2: 身份模型（Eco/Platform/RuntimeId/Digest/PackageIdentity）

**Files:**

- Create: `crates/mise-store/src/error.rs`、`crates/mise-store/src/identity.rs`
- Modify: `crates/mise-store/src/lib.rs`（放开 pub mod）
- Test: `crates/mise-store/tests/identity.rs`

**Interfaces:**

- Consumes: Task 1 的 crate 骨架
- Produces: `PackageIdentity` 及全部部件的 `Display`/`FromStr`（后续所有任务的身份货币）；规范文本格式：`python:numpy@4.2.2+python@3.10.14/macos-aarch64/sha256-<64hex>`（node 无运行时段：`node:@types/node@5.8.2/linux-x86_64-gnu/sha256-…`）

**验收标准：** `cargo test -p mise-store` 全绿；含 python/node(带作用域名)/非法输入三类 roundtrip 与 digest 校验用例。

- [x] Step 1: 写失败测试 `crates/mise-store/tests/identity.rs`：

```rust
use mise_store::{Digest, PackageIdentity, Platform, RuntimeId, Eco};

fn hex64(c: char) -> String { std::iter::repeat(c).take(64).collect() }

#[test]
fn identity_roundtrip_python() {
    let s = format!("python:numpy@4.2.2+python@3.10.14/macos-aarch64/sha256-{}", hex64('a'));
    let id: PackageIdentity = s.parse().unwrap();
    assert_eq!(id.eco, Eco::Python);
    assert_eq!(id.name, "numpy");
    assert_eq!(id.version, "4.2.2");
    let rt = id.runtime.unwrap();
    assert_eq!(rt, RuntimeId { eco: Eco::Python, version: "3.10.14".into() });
    assert_eq!(id.platform, "macos-aarch64".parse().unwrap());
    assert_eq!(id.to_string(), s);
}

#[test]
fn identity_roundtrip_node_scoped() {
    // npm 作用域包名内含 '/'，解析必须从右侧切分平台与 digest
    let s = format!("node:@types/node@5.8.2/linux-x86_64-gnu/sha256-{}", hex64('b'));
    let id: PackageIdentity = s.parse().unwrap();
    assert_eq!(id.name, "@types/node");
    assert!(id.runtime.is_none());
    assert_eq!(id.to_string(), s);
}

#[test]
fn identity_rejects_garbage() {
    assert!("python:numpy/macos-aarch64/sha256-aa".parse::<PackageIdentity>().is_err());
    assert!("ruby:rails@1.0.0/macos-aarch64/sha256-aaaa".parse::<PackageIdentity>().is_err()); // 未知生态
    assert!(format!("python:numpy@1.0.0/macos-aarch64/sha256-{}", hex64('c')).replace('a', "z").parse::<PackageIdentity>().is_err());
}

#[test]
fn digest_hashing_and_parsing() {
    let d = Digest::from_sha256_bytes(b"hello");
    assert_ne!(d.to_string(), Digest::from_sha256_bytes(b"world").to_string());
    assert_eq!(d, d.to_string().parse().unwrap());
    assert!("sha256-short".parse::<Digest>().is_err());
}

#[test]
fn platform_detect_matches_host_shape() {
    let p = Platform::detect();
    let s = p.to_string();
    assert!(s.starts_with("macos-") || s.starts_with("linux-"));
    if s.starts_with("linux-") { assert!(s.ends_with("-gnu") || s.ends_with("-musl")); }
}
```

- [x] Step 2: `cargo test -p mise-store` 确认编译失败（模块不存在）
- [x] Step 3: 实现 `error.rs`：

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("unknown ecosystem: {0}")]
    UnknownEco(String),
    #[error("invalid identity: {0}")]
    InvalidIdentity(String),
    #[error("invalid digest: {0}")]
    InvalidDigest(String),
    #[error("digest mismatch for {identity}: expected sha256-{expected}, got sha256-{actual}")]
    DigestMismatch { identity: String, expected: String, actual: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;
```

实现 `identity.rs`（要点，非逐字模板）：

- `Eco { Python, Node }`：`dir_name()`、`Display`（小写名）、`FromStr`（未知值 → `UnknownEco`）
- `Platform { os, arch, libc }`：`detect()` 用 `cfg!(target_os/arch/env)` 编译期探测；`Display` 为 `os-arch`（非 Linux）/`os-arch-libc`（Linux）；`FromStr` 接受两形态，Linux 缺 libc 段默认 `Gnu`
- `RuntimeId { eco, version }`：`Display` = `python@3.10.14`；`FromStr` 逆操作
- `Digest([u8;32])`：`from_sha256_bytes(&[u8]) -> Self`（sha2）；`hex()`；`Display` = `sha256-<hex>`；`FromStr` 要求 64 位十六进制；serde 手动实现为该字符串形态（锁文件可读）
- `PackageIdentity`：`Display` 按规范文本拼接；`FromStr` 解析顺序——先 `rsplitn(3, '/')` 取 digest/platform，再对剩余段：取首个 `':'` 前为 eco；含 `'+'` 则 `split_once('+')` 分出运行时段（两种生态的包名均不含 `'+'`，npm 作用域名含 `'/'` 与 `'@'`，版本用 `rsplit_once('@')` 切，保证 `@types/node@5.8.2` 解析正确）；另提供 `dir_name()` 返回 `name.replace('/', "+")`（npm 作用域名含 `/` 不能直接当目录名，`+` 在两种生态包名里都不合法，无碰撞）
- 全类型 `#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]`

- [x] Step 4: `cargo test -p mise-store` 全绿
- [x] Step 5: 提交：`feat(packages): add package identity model to mise-store`

---

### Task 3: 路径计算与 Store（contains/unpack）

**Files:**

- Create: `crates/mise-store/src/path.rs`、`crates/mise-store/src/store.rs`
- Modify: `crates/mise-store/src/lib.rs`
- Test: `crates/mise-store/tests/store.rs`

**Interfaces:**

- Consumes: Task 2 的 `PackageIdentity`
- Produces: `Store::open/entry_path/contains/unpack`（Task 4 的 refcount 挂在 Store impl 上；M1 的下载-落盘流程直接消费 `unpack`）

**验收标准：** `cargo test -p mise-store` 全绿，覆盖：路径形状、digest 不符拒收、成功解包出文件、幂等（二次 unpack 直接返回路径）、`.mise-complete` 标记语义。

- [x] Step 1: 失败测试 `crates/mise-store/tests/store.rs`：

```rust
use mise_store::{Digest, PackageIdentity, Store};

fn make_tgz() -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    {
        let mut b = tar::Builder::new(&mut enc);
        let mut h = tar::Header::new_gnu();
        h.set_size(5);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, "pkg/hello.txt", &b"hello"[..]).unwrap();
        b.finish().unwrap();
    }
    enc.finish().unwrap()
}

fn identity(digest: Digest) -> PackageIdentity {
    format!("python:numpy@4.2.2+python@3.10.14/macos-aarch64/{digest}").parse().unwrap()
}

#[test]
fn entry_path_shape() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir).unwrap();
    let id = identity(Digest::from_sha256_bytes(b"x"));
    let p = s.entry_path(&id);
    assert!(p.ends_with(format!("python/numpy/4.2.2/{id-ref-digest}").trim_end_matches('/')));
    assert!(p.starts_with(dir.path()));
}

#[test]
fn unpack_verifies_and_extracts() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir).unwrap();
    let bytes = make_tgz();
    let id = identity(Digest::from_sha256_bytes(&bytes));
    assert!(!s.contains(&id));
    let p = s.unpack(&id, &bytes).unwrap();
    assert!(p.join("pkg/hello.txt").is_file());
    assert!(s.contains(&id));
    // 幂等
    assert_eq!(s.unpack(&id, &bytes).unwrap(), p);
}

#[test]
fn unpack_rejects_digest_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir).unwrap();
    let bytes = make_tgz();
    let id = identity(Digest::from_sha256_bytes(b"other"));
    let err = s.unpack(&id, &bytes).unwrap_err();
    assert!(err.to_string().contains("digest mismatch"));
    assert!(!s.contains(&id));
}
```

（`entry_path_shape` 中 digest 段直接用 `id.digest.to_string()` 拼接断言，执行时写成精确形式。）

- [x] Step 2: 跑测试确认失败
- [x] Step 3: 实现：
  - `path.rs`：`pub fn entry_dir(root: &Path, id: &PackageIdentity) -> PathBuf` = `root/eco.dir_name()/id.dir_name()/id.version/id.digest.to_string()`
  - `store.rs`：`Store { pub root: PathBuf }`；`open` 建根目录；`contains` 判 `entry/.mise-complete` 是否存在；`unpack`：先 `Digest::from_sha256_bytes(bytes)` 比对（不符返回 `DigestMismatch`）→ 已 contains 则直接返回 → 解到 `root/.tmp-<pid>-<digest前8>`（flate2 GzDecoder + tar::Archive::unpack，tar 内路径穿越由 tar crate 的 unpack 防护）→ 写 `.mise-complete` 与 `.mise-meta.json`（identity 序列化）→ `fs::rename` 到目标；rename 失败（并发已存在）则删除 tmp 返回现路径
- [x] Step 4: `cargo test -p mise-store` 全绿
- [x] Step 5: 提交：`feat(packages): add store layout, unpack and digest verification`

---

### Task 4: 引用计数与 gc

**Files:**

- Create: `crates/mise-store/src/refcount.rs`
- Modify: `crates/mise-store/src/lib.rs`、`src/store.rs`（refcount 方法并入 `impl Store`，放 refcount.rs 内的独立 `impl Store` 块）
- Test: `crates/mise-store/tests/refcount.rs`

**Interfaces:**

- Consumes: Task 3 的 `Store`
- Produces: `add_ref/remove_ref/refs`、`gc(&Store) -> Result<GcReport>`；`GcReport { removed: Vec<PathBuf>, freed_bytes: u64 }`

**验收标准：** `cargo test -p mise-store` 全绿，覆盖：加引用→列举；移除至空→gc 删除目录并统计字节；仍有引用→gc 保留；陈旧 `.tmp-*` 目录被 gc 清掉；refs 文件缺失视为空。

- [x] Step 1: 失败测试（要点）：`refs` 文件为 `entry/.mise-refs.json`，结构 `{"projects":["/a","/b"]}`（排序去重）；`remove_ref` 删至空列表；`gc` 对 `remove` 后的条目断言 `removed.len()==1 && freed_bytes>0` 且目录消失；对仍有引用的条目断言目录还在；手工建 `root/.tmp-1-abcdef` 断言被 gc 移除。
- [x] Step 2: 跑测试确认失败
- [x] Step 3: 实现：`Refs { projects: Vec<String> }`（serde，读写走 serde_json，文件不存在 → default）；`add_ref` 插入排序去重；`remove_ref` 移除；`gc`：遍历 root 下两层定位 eco/name/version/digest 条目（`read_dir` 递归 4 层，只进目录），有 `.mise-complete` 且 refs.projects 为空 → 统计目录大小后删除；顺带清除 `root` 下 `.tmp-*` 目录；`dir_size` 手写递归求和（file len）。
- [x] Step 4: `cargo test -p mise-store` 全绿
- [x] Step 5: 提交：`feat(packages): add store refcounting and garbage collection`

---

### Task 5: resolver 图类型（拓扑排序/确定性/序列化）

**Files:**

- Create: `crates/mise-resolver/src/error.rs`、`crates/mise-resolver/src/graph.rs`
- Modify: `crates/mise-resolver/src/lib.rs`
- Test: `crates/mise-resolver/tests/graph.rs`

**Interfaces:**

- Produces: `DependencyRequest { eco: String, name: String, constraint: String }`、`ResolvedNode { eco, name, version, digest, deps: Vec<DependencyRequest> }`、`ResolvedGraph`；`sorted()`（规范序 (eco,name,version)）、`install_order()`（依赖在前，环→`ResolverError::Cycle`，悬空依赖→`MissingNode`）；serde JSON roundtrip（M1 锁文件 v2 的图段直接复用）
- 说明：`eco` 用 String（"python"/"node"）是分层决策（ARCHITECTURE.md 依赖方向节），类型化 `Eco` 在 glue 层转换

**验收标准：** `cargo test -p mise-resolver` 全绿，覆盖：菱形依赖顺序（C 在 B 前、B 在 A 前）、双向环报 `Cycle`、悬空依赖报 `MissingNode`、乱序输入经 `sorted()` 后逐字节相等（确定性）、serde roundtrip。

- [x] Step 1: 失败测试（要点）：构造 A→B,C；B→C 的三节点图，断言 `install_order()` 中 `pos("python:C") < pos("python:B") < pos("python:A")`（key 形如 `{eco}:{name}`）；A↔B 断言 `matches!(err, ResolverError::Cycle)`；节点声明依赖 `python:ghost` 但无该节点断言 `MissingNode`；`nodes` 乱序两份 `sorted()` 相等；`serde_json::to_string → from_str` roundtrip 相等。
- [x] Step 2: 跑测试确认失败
- [x] Step 3: 实现 `graph.rs`：petgraph `DiGraph<String, ()>`，节点 key `format!("{eco}:{name}")` 入 `HashMap<String, NodeIndex>`；边方向 **依赖 → 依赖者**（toposort 保证每条边 a→b 中 a 先出现，即被依赖者先装）；`ResolverError { #[error("dependency cycle involving {0}")] Cycle(String), #[error("node {0} referenced but absent")] MissingNode(String) }`
- [x] Step 4: `cargo test -p mise-resolver` 全绿
- [x] Step 5: 提交：`feat(packages): add resolver graph types with deterministic topo order`

---

### Task 6: M0 收口验证

**Files:** 无新增（如失败则回改）

**验收标准（全部当次运行并粘贴输出）：**

- [x] `cargo test -p mise-store -p mise-resolver` 全绿
- [x] `cargo clippy --workspace --all-features --all-targets -- -D warnings` 通过（AGENTS.md 铁律，无任何 allow；退出码 0，2026-10-06）
- [x] `mise run lint`：本机环境受阻——mise 的 npm 后端在含中文的仓库路径（`~/文件/…`）上 panic，无法安装 prettier 等 lint 工具（与 M0 改动无关，已单独记录）。以等效替代验证：`cargo fmt --check`（两 crate）绿 + `npx prettier --check`（全部新增 md）绿 + 全 workspace clippy 绿；完整 lint 留待 CI
- [x] 更新 PLAN 勾选、log 记录、HANDOFF 更新；向用户报告 M0 验收结果，请求 M1 细化许可

---

## 里程碑索引（M0 验收后逐个细化成上述任务粒度并追加）

| 里程碑 | 内容一句话    | 关键新增                                                                                                                                                                             |
| ------ | ------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| M1     | python 端到端 | mise-index crate（PyPI 客户端+缓存）、pubgrub+pep440/pep508 求解、wheel 产物平台匹配（zip 解包）、`src/packages/` 粘合层、venv 物化、lock v2、install/use CLI 语法、差分测试 harness |
| M2     | node 端到端   | npm registry 客户端、node-semver 适配、node_modules 物化、对照 npm 差分                                                                                                              |
| M3     | 项目命令族    | project init/attach/ls/status、projects.json 索引、outdated/upgrade 扩展                                                                                                             |
| M4     | 纵向生态      | go（MVS）、maven；store gc 接入 auto-prune                                                                                                                                           |
| M5     | 择机扩展      | sdist 构建、非精确版本可选层、Windows 重评估（新 ADR）                                                                                                                               |

## 执行方式确认

计划落档后用户选定执行方式：本会话逐任务执行 / 每任务派子代理执行后主会话审查。PLAN 的 checkbox 是唯一进度真相。

---

## M1 计划 — python 端到端（2026-10-06 追加，用户批准开工；沿用逐任务执行 + 逐任务提交）

> 粒度说明（记入 log）：M1 任务给出 Files/Interfaces/验收标准与关键代码；涉及第三方 crate 具体 API（pubgrub/reqwest/pep508 的 trait 签名）时，任务内含"读 registry 源码核对签名"这一显式步骤——这是执行步骤而非留空。M0 已验证的纪律（clippy 独立跑、真实退出码落盘）全程沿用。

## M1 文件结构图（新增部分）

```
Cargo.toml                          [改] members += crates/mise-index
crates/mise-index/
  src/lib.rs                        Index::open(cache_root)、re-export
  src/error.rs                      IndexError
  src/types.rs                      PackageMetadata/ArtifactDescriptor/DependencyConstraint/RuntimeInfo
  src/wheel.rs                      parse_wheel_filename + WheelTags + wheel_matches(平台匹配, 纯逻辑)
  src/pypi.rs                       PyPI JSON API 客户端 + 响应解析 + marker 过滤(pep508_rs)
  src/cache.rs                      元数据磁盘缓存(TTL)
  tests/wheel.rs, tests/pypi_parse.rs(离线 fixture)
crates/mise-store/
  src/store.rs                      [改] unpack 支持 zip(magic 检测分流)
src/packages/                       主 crate 新模块
  mod.rs                            模块声明 + PackageInstallRequest
  python.rs                         元数据→身份 映射、wheel 选择
  materialize.rs                    venv 物化(pyvenv.cfg/bin/site-packages 链接农场) + add_ref
  lockfile.rs                       mise.lock [package] 表读写
  install.rs                        编排: 请求→index→resolver→store→materialize→lock
src/cli/install.rs                  [改] `+` 绑定语法与裸包参数解析
src/config/                         [改] [packages."runtime"] 表解析
e2e/packages/install_python.sh      真实网络端到端
crates/mise-resolver/tests/differential/  差分测试 harness(uv 存在才跑)
```

## M1 任务清单

### Task 1 ✅(完成 2026-10-07): mise-index 骨架 + wheel 文件名解析（纯逻辑）

- Files: Create crates/mise-index/{Cargo.toml,src/lib.rs,src/error.rs,src/wheel.rs}, tests/wheel.rs; Modify 根 Cargo.toml
- Interfaces: `parse_wheel_filename(&str) -> Option<WheelFile>`；`WheelFile{name,version,tags}`；`WheelTags{python,abi,platform:Vec<String>}`（展开 cp310-cp310 这种重复段）
- 验收: `cargo test -p mise-index` 绿；覆盖 numpy cp310 macosx、纯 py3-none-any、非法名（段数不足/无 .whl）三类
- 步骤: ①失败测试（含 `pip-24.0-py3-none-any.whl`、`numpy-4.2.2-cp310-cp310-macosx_11_0_arm64.whl`、`a-1.0.0-cp310-cp310.whl`→None、`a.whl`→None）②实现 ③绿 ④clippy/fmt ⑤提交 `feat(packages): add mise-index with wheel filename parsing`

### Task 2 ✅(完成): wheel 平台匹配器（纯逻辑）

- Files: crates/mise-index/src/wheel.rs [改], tests/wheel.rs [改]
- Interfaces: `pub struct RuntimeInfo{pub major:u32,pub minor:u32}`（M1 只支持 CPython）；`pub fn wheel_matches(tags:&WheelTags, rt:&RuntimeInfo, platform:&mise_store::Platform)->bool`（依赖 mise-store 的 Platform，方向：index→store 合法，无环）
- 匹配规则（写死在实现，测试覆盖）: python 段 `py3`/`py2.py3` 匹配 major=3；`cp{maj}{min}` 精确匹配运行时；abi 段 `none` 恒真、`abi3` 对 cp≥3 恒真、`cp{maj}{min}` 精确；platform 段 `any` 恒真、`macosx_*_{arch}` 匹配 mac+对应架构（arm64 轮子在 mac-aarch64 恒可）、`manylinux_*_{arch}` 匹配 linux+对应架构、`musllinux_*_{arch}` 匹配 linux+musl、`linux_{arch}` 匹配 linux-gnu
- 验收: 测试矩阵 ≥10 断言（含 numpy CUDA 常见的 manylinux + cp310 组合、py3-none-any 全平台命中、musl 轮子不匹配 gnu）
- 步骤同 Task 1；提交 `feat(packages): wheel platform tag matching`

### Task 3 ✅(完成): PyPI 客户端与元数据类型

- Files: crates/mise-index/src/{types.rs,pypi.rs,cache.rs}, tests/pypi_parse.rs + tests/fixtures/numpy.json(节选真实响应)
- Interfaces: `Index::open(cache_root)`；`async fn metadata(&self, name, version) -> Result<PackageMetadata>`（缓存命中/过期刷新）；`PackageMetadata{name,version,requires_python,deps:Vec<DependencyConstraint>,artifacts}`；`DependencyConstraint{name,constraint}`（pep440 字符串）；marker 过滤：用 pep508_rs 的 `Requirement`+`MarkerEnvironment` 按 RuntimeInfo 求 python_version 求值后丢弃不适用依赖
- 依赖: reqwest(rustls,json)、pep440_rs、pep508_rs、serde/serde_json/thiserror；dev: tokio(rt,macros)
- 步骤含: 读 ~/.cargo/registry 下 pep508_rs 的 MarkerEnvironment 字段定义（核对构造方式）
- 验收: 离线 fixture 解析出 numpy 的依赖数组与 artifacts（含 sha256）；marker `; python_version < "3.11"` 在 rt=3.10 保留、3.11 丢弃；缓存写入/命中单测（tempdir）；网络请求仅集成测试手动跑
- 提交 `feat(packages): pypi metadata client with marker filtering`

### Task 4 ✅(完成): resolver pubgrub 求解器

- Files: crates/mise-resolver/src/{solve.rs,pyver.rs} [lib.rs 改], tests/solve.rs
- Interfaces: `solve(requests:&[DependencyRequest], locked:&ResolvedGraph, provider:&dyn MetadataProvider) -> Result<ResolvedGraph, ResolveFailure>`；`trait MetadataProvider{fn metadata(&self,name,version)->Option<PackageMetadataLite>}`（deps 是 pep440 约束串，由注入方给）；`PyVersion`（pep440 newtype，impl pubgrub Version 的 delegate 模式）
- 步骤含: 读 pubgrub crate 源码核对 Delegate/Version trait 精确签名后再写适配（记 log）
- 验收: 单测——单包精确锚定；A→B(范围)→C(范围) 传递求解取各自范围内确定版本；两包对 C 约束冲突时给出含双方约束来源的失败报告；locked 沿用旧选择（不无故升级）；确定性（乱序输入同输出）
- 提交 `feat(packages): pubgrub-based python resolver`

### Task 5 ✅(完成): store 支持 zip 解包（wheel 落位）

- Files: crates/mise-store/src/store.rs [改], tests/store.rs [改]；Cargo.toml += zip
- Interfaces: `unpack` 按 magic 分流：PK\x03\x04→zip 解包，1f 8b→tar.gz；其余报 InvalidIdentity 类错误（新增 InvalidArchive 变体）
- 验收: zip 构造→unpack→contains；混合格式判别
- 提交 `feat(packages): store unpack support for zip archives`

### Task 6 ✅(完成): 主 crate 集成 — [packages] 配置与 install 语法（CLI `+` 语法解析已备，`mise install` 参数路径接驳留 M2）

- Files: src/packages/{mod.rs,python.rs} 新建；src/cli/install.rs 改；src/config/ 探索后定点改（读 config_file 的 to_tool_request_set 模式后照做 to_package_requests）
- Interfaces: `PackageInstallRequest{runtime:RuntimeId, name, version}`；`[packages."python@3.10.14"]` 表→Vec<PackageInstallRequest>；`python@3.10.14+numpy@4.2.2` 语法→(工具请求, 包请求)；裸 `numpy@4.2.2` 在唯一 python 运行时下自动绑定、多运行时报错提示显式语法
- 验收: 主 crate 单测（参数解析矩阵）；`cargo check -p mise` 过
- 提交 `feat(install): parse package requests from cli and [packages] config`

### Task 7 ✅(完成): venv 物化器

- Files: src/packages/materialize.rs + tests
- Interfaces: `materialize_venv(config, runtime, graph, store) -> Result<PathBuf>`：生成 `<proj>/.mise/venvs/python-3.10.14/{pyvenv.cfg,bin/python→installs,lib/python3.X/site-packages/→store 条目顶层目录链接}`；store.add_ref(project_root)
- 验收: tempdir 单测目录形状+链接指向+refs 更新；真实 import 验证放 e2e
- 提交 `feat(packages): venv materialization over store`

### Task 8 ✅(完成): 锁文件 v2（边车 mise.packages.lock，见 log 偏差说明）

- Files: src/packages/lockfile.rs；读 src/lockfile.rs 后扩展
- Interfaces: 图序列化为 mise.lock `[package]` array-of-tables（eco/name/version/runtime/digest/deps）；读锁→ResolvedGraph；配置未变时按锁直装
- 提交 `feat(lockfile): record resolved package graph`

### Task 9 ✅(完成): 安装编排串联（真实 e2e 通过）

- Files: src/packages/install.rs；src/cli/install.rs 接线
- 流程: 请求→index.metadata→resolver.solve→逐节点 wheel_matches 选产物→下载→store.unpack→materialize→写锁→进度条（复用 ui）；未配置的包提示 mise use
- 提交 `feat(install): end-to-end python package installation`

### Task 10 ✅(完成): 差分测试 harness

- Files: crates/mise-resolver/tests/differential/{mod.rs,fixtures/*}
- 规则: uv 存在→`uv pip compile --python-version` 求参照，比对版本集合；缺失→skip（输出 SKIPPED）
- 提交 `test(packages): differential harness against uv`

### Task 11 ✅(完成): e2e（install_python_slow.sh）

- Files: e2e/packages/install_python.sh
- 断言: `mise install python@3.10.14+numpy@X` 后 venv 内 `python -c "import numpy"` 成功；第二项目同请求秒完成且 store 只有一份；删配置后 gc 回收
- 提交 `test(packages): e2e python+numpy install`

### Task 12 ✅(完成): M1 收口

- 全量验证同 M0 Task 6（真实退出码落盘）；PLAN/log/HANDOFF 更新；报告用户

## M2 计划 — node 端到端（2026-10-07 追加，用户批准开工；沿用逐任务执行+逐任务提交）

> ADR-0005（边车锁）与 ADR-0004 补充（node 组键统一 `name@version` 语法）随本计划落档。

### Task 1 ✅: ADR-0005 + 本计划落档

### Task 2 ✅(完成): 求解器生态泛化

- Files: crates/mise-resolver/src/{solve.rs,lib.rs}、tests/{solve.rs,differential.rs}、src/packages/install.rs
- Interfaces: `solve(requests, locked, source, eco: &str)`；trait `PyMetadataSource` 改名 `MetadataSource`（保留旧名作 deprecated 别名不值得，直接改调用点）
- 验收: 现有 21 个 resolver 测试全绿（传 eco="python"）；install.rs IndexSource 适配
- 提交 `refactor(packages): parameterize the solver by ecosystem`

### Task 3 ✅(完成): mise-store Digest 支持 sha512

- Files: crates/mise-store/src/{identity.rs,store.rs}, tests/identity.rs
- Interfaces: `enum Digest { Sha256([u8;32]), Sha512([u64;8]) }`；新增 `from_sha512_bytes`/`verify(&self, bytes)->bool`；Display/FromStr 同时接受 `sha256-<64hex>`/`sha512-<128hex>`；unpack 改用 `digest.verify(bytes)`（256/512 自动分流）
- 验收: 身份/解包测试新增 sha512 用例；原 sha256 路径零回归
- 提交 `feat(packages): support sha512 artifact digests`

### Task 4 ✅(完成): npm range 语义适配器

- Files: crates/mise-resolver/src/npmver.rs [新], tests/npmver.rs [新]；Cargo += node-semver
- Interfaces: `npmver::range_to_ranges(&str) -> Result<Ranges<NpmVersion>, String>`；`pub type NpmRanges = Ranges<node_semver::Version>`
- 支持子集（逐条有测试）: 精确 `1.2.3`；`^`（含 0.x 特例 ^0.2.3→[0.2.3,0.3)、^0.0.3→=0.0.3）；`~`；`1.2.x/1.2.*/1.2`；`*`；比较符组合 `>=1.2 <2`；hyphen `1.2.3 - 2.3.4`；`||` 并集；`latest/dist-tags` 拒绝（严格版原则）
- 预发布简化（记 log）: 预发布版本仅精确匹配可命中，范围一律不含（与 npm 默认一致）
- 提交 `feat(packages): npm semver range adapter`

### Task 5 ✅(完成): npm registry 客户端

- Files: crates/mise-index/src/npm.rs [新], tests/npm.rs [新]；Cargo += base64
- Interfaces: `npm::parse_npm_version_metadata(&Value) -> Result<PackageMetadata>`（dependencies map→DependencyConstraint，dist.tarball/dist.integrity(sha512 base64→hex)→单 artifact）；`npm::parse_npm_versions(&Value) -> Vec<String>`（versions 键）；Index 增 `npm_metadata/npm_versions`（缓存键 eco="node"，TTL 同 PyPI）
- 验收: 离线 fixture 单测（`typescript@5.x` 形状）+ integrity base64 解码 + dist 空依赖
- 提交 `feat(packages): npm registry metadata client`

### Task 6 ✅(完成): 主 crate node 路由

- Files: src/packages/{mod.rs,install.rs}；新增 node.rs（选择唯一 tarball artifact、无 requires_python）
- Interfaces: install_packages 按 runtime_name 分流：python 走 PyPI 源+pyver；node 走 npm 源+npmver；IndexSource 拆 PythonSource/NodeSource 两个 MetadataSource 实现
- 验收: 单测（select 逻辑矩阵在 node.rs）；cargo check
- 提交 `feat(packages): route node package installs through npm registry`

### Task 7 ✅(完成): node_modules 物化器

- Files: src/packages/materialize_node.rs [新]；mise-store 增 `hardlink_tree(src,dst)`（跨卷回退拷贝）
- 结构: `node_modules/.mise-store/<name>@<version>/node_modules/<name>/`=逐文件硬链接自 store 条目 package/；每个节点的依赖在其虚拟目录内 `node_modules/<dep>` 链接；顶层 `node_modules/<name>` 仅直连依赖；`node_modules/.bin/<bin>` 链 package.json bin 字段（读 dist 解出的 bin 表——M2 由 metadata 携带）
- 验收: tempdir 单测（目录形状、依赖内链、.bin、store refs、幂等）；真实 require 放 e2e
- 提交 `feat(packages): node_modules materialization over the store`

### Task 8 ✅(完成): CLI `+` 语法接驳

- Files: src/cli/mod.rs（预处理抽取含 '+' 的参数→OnceLock）、src/cli/install.rs（消费：Bound→先装运行时再装包；Bare 判定按注册表）、src/packages/mod.rs
- 验收: 手动矩阵 `mise install node@22.14.0+typescript@5.8.2`；`mise install python@3.10.14+numpy`（无版本→严格版错误）
- 提交 `feat(install): accept runtime+package bound arguments`

### Task 9 ✅(完成): pubgrub 日志过滤

- Files: src/logger.rs（按 target 前缀 pubgrub 降级到 trace）
- 验收: DEBUG 构建输出不再出现 unit_propagation 行
- 提交 `fix(logger): silence pubgrub solver trace in debug output`

### Task 10 ✅(完成): 差分（npm）

- Files: crates/mise-resolver/tests/differential.rs 增 node fixture case（本地 npm 存在才跑 `npm install --dry-run --json` 对照）
- 提交 `test(packages): differential coverage for npm ranges`

### Task 11 ✅(完成): e2e

- Files: e2e/packages/install_node_slow.sh（node22+typescript5 精确钉、node -e require 断言、跨项目共享、.bin/tsc --version）
- 提交 `test(packages): e2e node+typescript install`

### Task 12 ✅(完成): 收口

- 全量验证（真实退出码）+ 新 crate Cargo.toml 版本降特异性（"1.0.229"→"1" 等，AGENTS 依赖纪律）+ ARCHITECTURE 更新 + log/HANDOFF

## M3 计划 — 项目命令族 + 体验补全（2026-10-07 追加，用户批准开工；沿用既定模式）

> 范围：设计文档 M3 原定核心（project 命令族 + outdated 扩展）+ M2 遗留体验项（裸包名、use 写表、.bin、npm live 差分）。go/maven 留 M4。

### Task 1 ✅: 本计划落档

### Task 2: 裸包名自动绑定 ✅(完成)

- 规则（SPEC FR-2 后半）：`mise install numpy@1.2.2`（无 '+'）且名字不是已知 backend → 包候选；绑定到配置中唯一受支持生态的运行时（python|node）；零个→提示先声明运行时；多个→报错要求 `+` 显式语法
- Files: src/cli/install.rs（run 前分类 ToolArg：`ta.ba.backend().is_err()` → 包）、单测
- 提交 `feat(install): bind bare package names to the sole package runtime`

### Task 3: mise use 写 [packages] ✅(完成)

- Files: src/config/config_file/mise_toml.rs（add_package 方法插入表）、src/cli/use.rs（消费 CLI_PACKAGE_ARGS 写入后触发安装）
- 提交 `feat(use): persist package pins from bound arguments`

### Task 4: node_modules/.bin ✅(完成)

- Files: src/packages/materialize_node.rs（读物化后 package.json 的 bin 字段（字符串或映射），.bin/<名> 符号链接）
- e2e 断言 `mise x -- node_modules/.bin/tsc --version`
- 提交 `feat(packages): link package bin entries into node_modules/.bin`

### Task 5: 项目命令族 ✅(完成)

- Files: src/cli/project.rs（新命令：init [PATH] --python --node --package；attach；ls；status）、projects.json 索引（dirs::STATE，init/attach 登记，损坏即重建扫盘）、cli/mod.rs Commands 枚举
- 项目=目录树+mise.toml（运行时向上查找既有逻辑，不动）
- 提交 `feat(project): init, attach, ls and status commands`

### Task 6: outdated 扩展 ✅(完成)

- Files: src/cli/outdated.rs（列出包组：配置钉版本 vs registry 最新稳定版，仅提示不自动改——严格版原则）
- 提交 `feat(outdated): show newer versions for pinned packages`

### Task 7: npm live 差分 ✅(完成)

- Files: crates/mise-index/tests/npm_live.rs（#[ignore] 网络测试：`npm view <pkg>@<ver> dependencies --json` 稳定输出 vs 我们的 npm_metadata 依赖集）
- 提交 `test(packages): live differential against npm view`

### Task 8: e2e ✅(完成)

- e2e/packages/bare_and_bin_slow.sh（裸名装包、.bin/tsc）；e2e/packages/project_commands.sh（init/attach/ls/status，非 slow）
- 提交 `test(packages): e2e bare names, bin links and project commands`

### Task 9: 收口 ✅(完成)

- `mise run render`（usage/completions/docs 快照）+ 全量验证（四 crate 测试/clippy/lint 真实退出码）+ ARCHITECTURE 更新 + log/HANDOFF

## M4 计划 — go/maven 纵向生态 + store gc 自动化（2026-10-07 追加，用户批准开工）

> 设计文档定位：go/maven 采用"存储层吸收"而非"解析层替换"——go.mod/pom.xml 继续作为包声明与锁（它们本身设计良好），mise 接管全局存储位置（GOMODCACHE / maven local repo）并编排安装。这使跨项目共享承诺对 go/maven 同样成立，且改造成本最低。

### Task 1: Go GOMODCACHE 集成 ✅(完成)

- Files: src/packages/go.rs [新]，src/packages/install.rs（路由），src/packages/mod.rs
- 内容：`install_go_modules`：检测项目 go.mod → 设 `GOMODCACHE=<store>/go/mod` → 运行 `go mod download all`（经 mise 安装的 go）→ store refs 登记
- 验收：go 项目 `mise install` 后 GOMODCACHE 内有模块；第二项目同依赖零下载
- 提交 `feat(packages): route go module downloads through the mise store`

### Task 2: Go env 集成 ✅(完成)

- Files: src/plugins/core/go.rs（env_var 新增 GOMODCACHE → store 路径）
- 验收：`mise x go -- go env GOMODCACHE` 指向 store；`go build` 正常使用缓存
- 提交 `feat(go): set GOMODCACHE to the mise store when go is active`

### Task 3: Maven 本地仓库集成 ✅(完成)

- Files: src/packages/maven.rs [新]，install.rs 路由
- 内容：检测 pom.xml → 设 `-Dmaven.repo.local=<store>/maven`（MAVEN_ARGS）→ `mvn dependency:go-offline` → refs
- 验收：mvn 项目 `mise install` 后依赖在 store；编译通过
- 提交 `feat(packages): route maven dependencies through the mise store`

### Task 4: store gc 接入 auto-prune ✅(完成)

- Files: src/packages/mod.rs（gc 周期调用）、src/cache.rs
- 内容：`mise install` 尾部触发 `store gc`（频率：每日一次，STATE 记 last-run）
- 提交 `feat(packages): auto-prune store entries on install`

### Task 5: e2e + 收口 ✅(完成)

- e2e/packages/install_go_slow.sh（go.mod 两项目共享 GOMODCACHE）；maven 若本机有 mvn 则 e2e 否则跳过说明
- 全量验证 + ARCHITECTURE + log/HANDOFF

## M5 计划 — 补齐边角 + 交付准备（2026-10-07 追加；用户批准：删除 Windows，其余全做）

### Task 1 ✅: 本计划落档

### Task 2: PEP 517 源码构建

- 当无兼容 wheel 时回退 sdist：`pip wheel --no-deps --wheel-dir <tmp>` 用 mise 安装的 python → 产物 wheel 进 store
- 安全默认：仅在无预编译替代时触发 + 显示警告"building from source"
- Files: src/packages/install.rs（select_artifact 回退 + build_from_source fn）
- 提交 `feat(packages): build python sdists when no wheel matches`

### Task 3: node-gyp 原生模块

- npm 包含 binding.gyp 时：物化后执行 `node-gyp rebuild`（用 mise 安装的 node）
- Files: src/packages/materialize_node.rs（post-materialize hook）
- 提交 `feat(packages): build node native modules after materialization`

### Task 4: 范围约束可选层 + mise upgrade

- 组键非精确（`[packages."python@3.10"]` 无 patch）→ 允许；解析时锁定具体版本写锁
- `mise upgrade`：丢弃锁偏好 → 重新求解 → 更新锁
- Files: packages/mod.rs（组键宽松解析）、cli/upgrade.rs、install.rs（upgrade flag）
- 提交 `feat(packages): optional range constraints and mise upgrade`

### Task 5: 锁文件统一

- 边车 mise.packages.lock 内容迁入 mise.lock 的 [package] 表；读双源（先主后边车）；写主
- ADR-0006 记录
- Files: lockfile.rs、packages/lockfile.rs
- 提交 `feat(lockfile): unify package graph into mise.lock`

### Task 6: mise store ls

- 生态分组 + 大小 + 引用项目
- Files: cli/store.rs [新] 或 project.rs 扩展
- 提交 `feat(store): human-readable store listing`

### Task 7: crate publish 兼容

- 三个新 crate 去 publish=false；主 crate path 依赖 version 兼容
- 提交 `chore: make new crates publishable`

### Task 8: 文档 + aube issue + 性能

- docs/dev-tools/packages.md 使用指南
- aube 上游 issue 草稿（env::vars panic）
- hook-env 包图校验开销测量
- 提交 `docs(packages): usage guide`

### Task 9: 收口

- 全量验证（四 crate + e2e 全家 + lint + clippy）+ ARCHITECTURE/SPEC 终稿 + log/HANDOFF
