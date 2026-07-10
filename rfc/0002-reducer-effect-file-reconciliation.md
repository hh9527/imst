# RFC 0002: 两级配置文件持续调谐

## 摘要

本 RFC 提议建立 `imst daemon` 的第一套 reducer/effect 持续调谐模型，并用两级动态配置
文件验证这套模型：

```text
环境变量
    -> 单一顶层配置文件
    -> 动态 WatchList
    -> 多个用户意图配置文件
    -> 合并后的 Intent
```

两个环境变量在 daemon 运行期间不可变：

- `IMST_CONFIG` 指向顶层配置文件。
- `IMST_DATA` 指向 daemon 完全、唯一控制的数据目录。

环境变量指定的是位置，不保证文件内容不变。顶层配置文件和它订阅的用户意图文件都会
持续变化。daemon 必须观察变化、保留 last-known-good 数据、在错误修复后自愈，并在顶层
配置移除订阅时回收该文件的 watch 和 intent contribution。

两类文件共享同一个以 `FileSpec` marker 类型参数化的 `FileState`、`FileEvent` 和
`FileEffect` 状态机；它们不同的下游调谐仍由顶层 `AnyEvent` 明确路由。

本 RFC 不实现 package 安装、下载、prune 或数据目录 GC。它验证的是这些能力未来需要依赖
的动态目标、状态调谐和反向回收骨架。

## 动机

RFC 0001 把安装描述为一次由命令入口驱动的 action pipeline。这足以验证 package spec、
revision 和 marker，但不适合 daemon 的长期模型。

`imst` 的输入不是一次性任务，而是持续变化的部署目标。管理员可以修改顶层配置；被订阅
的用户意图文件也可以独立变化。daemon 应持续使自己的观察和派生状态向这些输入收敛，
而不是把配置当作消费后删除的命令队列。

本 RFC 关注以下问题：

- 如何区分运行期不可变的 bootstrap 路径和持续变化的文件内容。
- 如何用同一个状态机加载两种结构化配置文件。
- 如何区分 effect 已提交和真正开始工作。
- 如何用 stat 做读取短路优化，而不把 mtime 当作部署目标身份。
- 如何在加载失败时继续使用 last-known-good 数据。
- 如何合并频繁文件通知，并用低频 fallback reload 弥补 inotify 丢失通知。
- 如何由顶层配置的 subscription set 驱动 intent-source 的发现和回收。

## 指南级说明

daemon 启动时读取环境变量：

```text
IMST_CONFIG=/etc/imst/config.json
IMST_DATA=/opt/imst/data
```

RFC 0002 首版只支持 JSON 配置。`IMST_CONFIG` 未设置时默认使用 `/etc/imst/config.json`；
具体默认格式选择仍见未决问题。

`IMST_DATA` 未设置时默认为 `/opt/imst/data`。daemon 对该目录具有完全、唯一控制权。初始
布局假定为：

```text
/opt/imst/data/
├── dl/
├── pkgs/
└── tmp/
```

顶层配置通过 `subscribe` 字段声明需要观察的用户意图文件：

```json
{
  "subscribe": [
    "/path/to/user1/intent.json",
    "/path/to/user2/intent.json"
  ]
}
```

用户意图文件包含 package spec 列表：

```json
{
  "packages": [
    {
      "name": "foo",
      "items": []
    }
  ]
}
```

两种文件在 RFC 0002 首版均使用 JSON。文件先反序列化成类型化数据，完成领域校验和结构
复用，再计算确定性 digest。输入空白和无语义顺序不直接参与 digest。

file watcher 始终观察顶层配置路径，并动态观察当前 subscription set。文件通知不执行
stat 或读取，只通过 keyed debounce 以 5 秒 timeout 投递类型化 `ReloadRequested` event。

`ReloadRequested` 从 `FileState` 快照 `prev_stat` 并声明 reload effect。handler 开始时投递
`ReloadStarted`，随后执行 stat：

```text
current_stat == prev_stat
    -> 短路内容读取
    -> ReloadFinished { data: None, error: None }

current_stat != prev_stat
    -> 读取原始字节
    -> ReloadFinished { data: Some(Vec<u8>) }

加载失败
    -> ReloadFinished { error: Some(...) }
```

reducer 收到原始字节后，对当前 `S::Data` 调用 `reuse_update`。该方法完成反序列化、校验
和结构复用，并返回领域数据是否变化。变化时 reducer 再调用 `update_digest`。读取、解析
或 reuse 失败都只更新错误观察，不替换 last-known-good stat/data。首次启动时
last-known-good 是合法 empty 数据；因此顶层配置或单个用户意图文件出错都不会使 daemon
OOS。文件修复后，持续 reload 会自动清除错误并推进有效数据。

顶层配置成功变化时，daemon 调谐 WatchList：新增路径开始观察并初始加载；删除路径停止
观察并立即退出有效 intent source set。这个反向过程是 RFC 0002 中的 subscription GC。

## 参考级说明

### Bootstrap 与数据目录

```text
BootstrapConfig {
    config_path: Path,
    data_path: Path,
}
```

`BootstrapConfig` 在 daemon 启动时构造，运行期间不可变。环境变量改变不要求在线生效，
需要重启 daemon。

`config_path` 和它指向的文件内容必须区分：path 不变，文件内容可变。

`data_path` 指向 daemon-owned 目录。普通用户、顶层配置和用户意图文件都不能直接管理其中
内容。本 RFC 只固定 `dl`、`pkgs` 和 `tmp` 三个候选子目录，不固定它们的长期布局和 GC
协议。

### 配置数据类型

```text
TopConfigData {
    subscribe: Set<Path>,
    digest: Sha256Digest,
}

UserIntentData {
    packages: Vec<PackageSpecItem>,
    digest: Sha256Digest,
}

PackageSpecItem {
    spec: Arc<PackageSpec>,
    digest: Sha256Digest,
}
```

`TopConfigData.subscribe` 的规整化至少包括：

- path 必须是 UTF-8 absolute path。
- path 必须 normalized，不包含 `.` 或 `..` segment。
- 重复 path 去重。
- 集合使用确定性顺序编码。

`UserIntentData.packages` 保留具有领域语义的顺序。每个 `PackageSpecItem` 使用 Arc 持有
规整化后的 `PackageSpec`，并缓存其确定性摘要。UserIntentState、Merged Intent 和后续
调谐通过 clone `PackageSpecItem` 共享同一不可变 package，避免深拷贝 spec。这里的
`Sha256Digest` 表示已经完成的 32-byte 摘要，不是可继续写入的 SHA-256 hasher state。
PackageSpec 的具体校验规则可以复用 RFC 0001。

`UserIntentData.digest` 由下级 `PackageSpecItem.digest` 二次 SHA-256 得到，不再重新编码或
遍历完整 `PackageSpec`：

```text
user_intent_digest = sha256(
    domain("imst:user-intent:v1")
    || u64_be(packages.len())
    || packages[0].digest
    || packages[1].digest
    || ...
)
```

package 顺序属于 UserIntent 语义，因此子 digest 按 `packages` 顺序输入。domain separation
防止不同领域层级意外共享同一种摘要输入；固定宽度的数量编码使空列表和序列边界明确。
二次摘要只用于高效传播变更，不能代替复用 `Arc<PackageSpec>` 前的完整值比较。

两种 empty last-known-good 为：

```text
TopConfigData { subscribe: {}, digest: hash(empty) }
UserIntentData {
    packages: [],
    digest: sha256(domain("imst:user-intent:v1") || u64_be(0)),
}
```

empty 数据具有确定性 digest，是正常有效状态，不是错误或 OOS 状态。

### JSON

RFC 0002 首版只解析 JSON。配置路径应使用 `.json` 扩展名；其他扩展名返回
`UnsupportedFormat` error。未来增加其他格式时可以扩展文件加载边界，不改变本 RFC 的
reducer/effect 状态语义。

叶子对象和 TopConfig digest 来自规整化后的类型化数据：

```text
digest = sha256(canonical_encoding(normalized_data))
```

digest 使用 canonical JSON 或其他内部确定性编码计算，不能直接 hash 原始文件字节。
UserIntent 是例外：其顶级 digest 按上一节定义，由有序的 PackageSpecItem digest 聚合得到。

未知字段、字段顺序和具体 canonical encoding 的长期兼容策略留给后续 RFC。本 RFC 的实现
必须至少保证同一版本程序内结果确定。

### FileSpec

两类文件通过 marker spec 关联 key 和可复用更新的 runtime data：

```text
FileSpec {
    type Key
    type Data
}

TopConfigSpec:
    Key = ()
    Data = TopConfigData

IntentConfigSpec:
    Key = Path
    Data = UserIntentData
```

Rust 参考 shape：

```rust
trait FileSpec: Send + 'static {
    type Key: Clone + Send + 'static;
    type Data: ReuseUpdate
        + Default
        + Send
        + 'static;
}
```

runtime data 自己负责从新字节进行事务性复用更新：

```rust
trait ReuseUpdate {
    fn reuse_update(
        &mut self,
        new_bytes: &[u8],
    ) -> Result<bool, FileLoadError>;

    fn update_digest(&mut self);

    fn digest(&self) -> &Sha256Digest;
}
```

`reuse_update` 返回 false 表示新文件已成功解析和校验，但领域数据没有变化；返回 true
表示 self 已更新。返回 Err 时 self 必须完全保持不变。实现必须先在局部变量中完成解析、
校验、复用和比较，最后才一次性替换字段。

对用户意图，reuse_update 优先复用当前 `PackageSpecItem` 中值相同的 Arc，只为新增或变化
的 spec 创建新 Arc：

```rust
fn reuse_packages(
    current: &[PackageSpecItem],
    incoming: Vec<PackageSpec>,
) -> Result<Vec<PackageSpecItem>, FileLoadError> {
    let previous_by_digest = index_packages_by_digest(
        current,
    )?;

    incoming
        .into_iter()
        .map(|package| {
            package.validate()?;
            let digest = package.digest()?;

            if let Some(previous) = previous_by_digest
                .get(&digest)
                .and_then(|candidates| {
                    candidates
                        .iter()
                        .find(|candidate| candidate.spec.as_ref() == &package)
                })
            {
                return Ok(previous.clone());
            }

            Ok(PackageSpecItem {
                spec: Arc::new(package),
                digest,
            })
        })
        .collect()
}
```

digest 按 `PackageSpec` 的值计算，不能使用 Arc 地址或引用计数。digest 只用于缩小 previous
candidate 范围，复用前还必须比较完整 PackageSpec 值，避免摘要碰撞导致错误复用。

reducer 在 `reuse_update` 返回 true 后调用 `update_digest`。对于 UserIntent，该调用只对
有序的 `PackageSpecItem.digest` 序列做二次 SHA-256，不重新遍历或序列化 PackageSpec。
返回 false 时不重新计算摘要，也不触发下游领域调谐。

```rust
struct TopConfigSpec;
struct IntentConfigSpec;
```

`FileSpec` 只抽象文件加载协议，不包含下游领域行为。TopConfig 更新 WatchList、UserIntent
更新 merged intent，仍由顶层 reducer 分别处理。

### FileState

```text
FileState<S: FileSpec> {
    stat: Option<Stat>,
    stage: LoaderStage,
    invalidated: bool,
    value: S::Data,
    error: Option<FileLoadErrorState>,
}

LoaderStage = Idle | Submitted | Working

FileLoadErrorState {
    at: u64,
    stat: Option<Stat>,
    error: FileLoadError,
}
```

`stat` 是当前 last-known-good 数据对应的文件状态。只有成功接受新数据时才推进。失败不能
推进 `stat`，否则后续 reload 可能错误短路，阻止自动恢复。

`value` 由 FileState 独占，reducer 可以直接调用
`value.reuse_update(new_bytes)`。Arc 只出现在 `UserIntentData` 的
`PackageSpecItem` 层，使同一文件的不同有效版本和 Merged Intent 能细粒度共享未变化的
package，而不共享或 copy-on-write 整个 FileState data。

`stage` 语义：

- `Idle`：没有 loader 工作。
- `Submitted`：reload effect 已提交，但 handler 尚未开始。
- `Working`：handler 已经开始执行。

本 RFC 保留这三个 variant，不设计取消、Retiring 或 operation identity。

`invalidated` 表示 Submitted/Working 期间又到达一个 `ReloadRequested`。它是纯 state
latch，不表示 debounce service 中已经存在 pending request。completion 消费并清除它，
用它选择下一次 reload 的 timeout。

### FileEvent 与 FileEffect

```text
FileEvent<S: FileSpec> =
    ReloadRequested {
        key: S::Key,
    }
  | ReloadStarted {
        key: S::Key,
    }
  | ReloadFinished {
        key: S::Key,
        at: u64,
        stat: Option<Stat>,
        error: Option<FileLoadError>,
        data: Option<Vec<u8>>,
    }

FileEffect<S: FileSpec> =
    Reload {
        key: S::Key,
        path: Path,
        prev_stat: Option<Stat>,
    }
```

顶层保持封闭：

```text
AnyEvent =
    TopConfig(FileEvent<TopConfigSpec>)
  | IntentConfig(FileEvent<IntentConfigSpec>)
  | Intent(IntentEvent)

AnyEffect =
    TopConfig(FileEffect<TopConfigSpec>)
  | IntentConfig(FileEffect<IntentConfigSpec>)
  | FileWatch(FileWatchEffect)
  | DebouncedKeyEvent {
        key: String,
        timeout: Duration,
        event: Box<AnyEvent>,
    }
```

`TopConfigSpec::Key = ()`，因为系统只有一个顶层配置文件，其 path 来自 bootstrap。
`IntentConfigSpec::Key = Path`，因为 subscription path 是多个用户意图状态的稳定身份。

### Event 与 Effect 边界

`AnyEvent` 提供同步 reducer：

```text
AnyEvent.reduce(self, state: &mut State, effects: &mut Effects)
```

只有 reducer 可以修改 State。reducer 不访问文件系统、timer 或网络。

`AnyEffect` 提供异步 handler：

```text
async AnyEffect.apply(self, emitter: EventEmitter, ctx: &Ctx)
```

effect 和 service 不能直接修改 State，只能投递 event。

`EventEmitter` 支持 immediate 和 keyed debounced 投递：

```text
EventEmitter.emit(event)

EventEmitter.emit_debounce(
    event,
    timeout,
    key,
)
```

sender 支持共享并发投递，因此方法使用 `&self` 即可。`DebouncedKeyEvent` effect 的 apply
只调用 `EventEmitter::emit_debounce`，不自行 sleep。

### Keyed debounce

固定 debounce service 为每个 String key 保存：

```text
Pending {
    deadline,
    event,
}
```

相同 key 的新请求替换 event，并使用最新 timeout 重置 deadline；不同 key 独立。deadline
到期后删除 pending slot，并把最后一个 event 投回 immediate event queue。

建议 key：

```text
config:reload
intent-config:reload:<normalized-path>
intent:update
```

key 只承担 runtime 分组，不表达领域类型。领域类型由 `AnyEvent` variant 保证。key 只能由
daemon 内部生成，不能直接来自不可信输入。

### 通用 loader 状态机

`ReloadRequested`：

| Stage | 状态变化 | Effect |
| --- | --- | --- |
| Idle | stage=Submitted, invalidated=false | `Reload { prev_stat: state.stat }` |
| Submitted | invalidated=true | 无 |
| Working | invalidated=true | 无 |

`ReloadStarted` 只允许在 Submitted，迁移到 Working。

handler 必须先投递 `ReloadStarted`，再执行 stat 和可选读取，最后恰好投递一个
`ReloadFinished`。handler 不解析 JSON，不执行领域校验或结构复用，也不计算 digest。
`Vec<u8>` 通过 event queue 移动所有权，不要求复制字节内容。同一个 producer 的事件顺序
必须保持。

`ReloadFinished` 合法组合：

| stat | error | data | 语义 |
| --- | --- | --- | --- |
| 与 prev_stat 相同 | None | None | stat 短路，未读取 |
| None | Some | None | stat 失败 |
| Some | None | Some | 原始文件读取成功，等待 reducer 解析 |
| Some | Some | None | stat 后的类型或读取失败 |

以下组合非法：

- data 和 error 同时为 Some。
- stat 为 None 但 data 为 Some。
- stat、error、data 同时为 None。

completion 只允许在 Working，完成后进入 Idle 并清除 invalidated。

结果应用：

- `data=Some(bytes)`：reducer 调用 `state.value.reuse_update(&bytes)` 解析 JSON。返回
  `Ok(true)` 时再调用
  `state.value.update_digest()`，推进 last-known-good stat、清除 error，并触发对应的下游
  调谐；返回 `Ok(false)` 时仍推进 stat 并清除 error，但不更新 digest，也不触发下游调谐。
- 读取失败，或 `reuse_update` 在反序列化、校验、结构复用期间返回错误：保留
  last-known-good stat/value，只替换 error observation。
- short-circuit：不修改 stat/value/error。

反序列化、校验和结构复用是确定性的内存计算，可以在 reducer 中执行。RFC 0002 假定配置文件
规模足够小，不会长时间阻塞 event loop；如果未来数据规模证明该假设不成立，可以增加
独立 CPU effect，但不改变本 RFC 的状态语义。

completion reload：

- completion 在 `invalidated == true` 时，以 1 秒 timeout debounce 一个
  `ReloadRequested`。
- completion 在 `invalidated == false` 时，以 30 秒 timeout debounce 一个
  `ReloadRequested`。
- 两条规则都适用于 stat short-circuit、加载成功和加载失败。
- completion 在同一次 reducer 转换中读取并清除 invalidated，再声明对应的 debounce
  effect。
- file watcher 的正常 inotify 通知使用 5 秒 timeout 和同一个 key，因此会替换 30 秒
  fallback deadline，优先触发 reload。invalidated completion 的 1 秒 deadline 则用于
  已知在工作期间发生变化时更快追赶。

这会形成低频持续 stat polling。稳定文件会在 stat short-circuit 后继续安排下一次 30 秒
检查，但不会读取和解析内容。fallback 只用于弥补 inotify 丢失通知，正常变化发现仍以
inotify 为主。

### 顶层配置调谐

file watcher 固定观察 `BootstrapConfig.config_path`。启动和文件通知都使用：

```text
emit_debounce(
    TopConfig::ReloadRequested { key: () },
    5s,
    "config:reload",
)
```

TopConfig 加载失败只更新 error，当前 subscription roots 不变。

TopConfig 加载成功且 digest 变化时，reducer 用新 subscribe set 替换 WatchList，并声明
FileWatch reconcile effect。

### WatchList 与 subscription GC

```text
WatchList = current TopConfigData.subscribe
```

新增 path：

- 在 `intent_configs` 中创建 empty `FileState<IntentConfigSpec>`，或复用已有缓存 state。
- 向 file watcher 注册 path。
- debounce 初始 `IntentConfig::ReloadRequested`。

删除 path：

- 从有效 WatchList 移除。
- 从 file watcher 注销 path。
- 立即从 merged intent source set 排除。
- 迟到的 ReloadRequested 首先检查 WatchList，不在集合中则 Noop。
- 已经 Working 的结果可以完成，但不能重新贡献到 merged intent。

这构成 subscription GC：TopConfig subscription set 是 intent-source root set。daemon 不删除
用户拥有的配置文件，也不在本 RFC 中删除 `IMST_DATA` 下的 package 或 download。

### 用户意图调谐

每个 WatchList path 使用：

```text
FileEvent<IntentConfigSpec>
FileEffect<IntentConfigSpec>
FileState<IntentConfigSpec>
```

用户意图加载失败只更新该 source 的 error，保留其 last-known-good value。其他 source 和
daemon 全局服务不受影响。

用户意图加载成功且 digest 变化时，reducer debounce 无 payload `UpdateIntentEvent`：

```text
DebouncedKeyEvent {
    key: "intent:update",
    timeout: intent_update_timeout,
    event: UpdateIntentEvent,
}
```

### Merged Intent

`UpdateIntentEvent` 只读取当前 WatchList 中各 source 的 last-known-good value：

```text
IntentState {
    sources: Map<Path, Vec<PackageSpecItem>>,
    digest: Sha256Digest,
}
```

本 RFC 先按 source 保存合并视图，不解决不同文件中同名 package 的冲突、依赖或版本求解。
构造该视图只 clone `PackageSpecItem`，其中的 `Arc<PackageSpec>` 避免深拷贝 package spec。
IntentState digest 按 source path 和解引用后的 package 值确定性计算，不依赖 Arc identity。
WatchList 删除 path 时，即使缓存 state 仍存在，该 path 也不能进入 IntentState。

### Runtime 与任务监督

```text
top
├── event loop + State
├── effects: JoinSet
└── services: JoinSet
    ├── file_watcher
    └── keyed_debouncer
```

effect 是有限任务，正常完成是预期行为。service 是长期任务，正常运行期间意外返回或 panic
必须由 top 识别为 service failure。

top 持有 immediate event receiver、State 和 reducer loop。每次 reduce 后，把声明的具体
AnyEffect 加入 effects JoinSet。

## 不变量

1. BootstrapConfig 在 daemon 运行期间不变。
2. IMST_DATA 下内容只由 daemon 管理。
3. 只有 reducer 可以修改 State。
4. effect 和 service 只能通过 EventEmitter 返回观察。
5. 两类文件使用同一 FileState/FileEvent/FileEffect 状态机。
6. 每个 FileState 同时最多有一个 reload handler。
7. LoaderStage 只按 Idle -> Submitted -> Working -> Idle 迁移。
8. Reload effect 必须携带 reducer 快照的 prev_stat，但不携带 previous Data。
9. 文件失败不能替换 last-known-good stat/value。
10. TopConfig 失败不能撤销 subscription roots。
11. UserIntent 失败不能撤销该 source 的最后有效贡献。
12. 只有成功 TopConfig 数据可以改变 WatchList。
13. Merged Intent 只能包含当前 WatchList 中的 source。
14. 同 key debounce 只投递安静期内最后一个 event。
15. invalidated completion 必须以 1 秒 timeout 续订 ReloadRequested。
16. 未 invalidated completion 必须以 30 秒 timeout 续订 ReloadRequested。
17. completion 必须在同一次 reducer 转换中清除 invalidated 并声明 debounce effect。
18. inotify notification 必须以 5 秒 timeout 使用同一个 key，可以替换 30 秒 fallback
    deadline。
19. UserIntentData 中的每个 PackageSpec 必须使用 Arc 共享；Merged Intent 不得深拷贝
    PackageSpec。
20. PackageSpec、UserIntent 和 IntentState digest 只能依赖解引用后的值，不能依赖 Arc 地址
    或引用计数。
21. reuse_update 必须是事务性的；返回 Err 时 Data 完全不变，值相同的 PackageSpec 应复用
    已有 Arc。
22. ReloadFinished 只携带原始 Vec<u8>；反序列化、校验、结构复用和 digest 更新必须由
    reducer 基于当前 last-known-good Data 完成。
23. FileState 直接持有 S::Data；Arc 只用于 PackageSpecItem.spec。
24. UserIntentData digest 必须由带 domain separation 和 package 数量的有序
    PackageSpecItem digest 序列二次 SHA-256 得到。

## 验收标准

1. 支持从环境变量构造不可变 BootstrapConfig，并提供约定默认值。
2. 初始化并独占管理 IMST_DATA/{dl,pkgs,tmp}。
3. JSON TopConfig 能解析为类型化数据和确定性 digest。
4. JSON UserIntent 能解析为类型化数据和确定性 digest。
5. TopConfig 和 UserIntent 使用 FileSpec 参数化的公共状态机。
6. loader stage 能观察 Idle、Submitted 和 Working。
7. prev_stat 相同时 handler 不读取文件内容。
8. 加载失败保留 last-known-good，并持续提供重试和自愈路径。
9. 首次加载失败时 empty last-known-good 使 daemon 继续运行。
10. TopConfig subscription 新增 path 后开始 watch 并初始加载。
11. TopConfig subscription 删除 path 后停止 watch，并从 IntentState 排除。
12. TopConfig 加载错误不改变当前 WatchList。
13. 单个 UserIntent 加载错误不影响其他 source。
14. keyed debounce 支持独立 key、独立 timeout 和替换 pending event。
15. FileWatch notification 不执行 stat 或读取，只 debounce ReloadRequested。
16. invalidated completion 以 1 秒、未 invalidated completion 以 30 秒续订 reload，两者
    都包括 stat 短路。
17. inotify notification 使用相同 key 和 5 秒 timeout，能够提前替换 30 秒 fallback。
18. UserIntent 加载后，每个 PackageSpec 只移动进入一个 Arc；生成 IntentState 时只 clone
    Arc，不深拷贝 PackageSpec。
19. UserIntent 新有效版本中值未变化的 PackageSpec 复用上一版本的 Arc。
20. handler 读取成功后通过 ReloadFinished 移动原始 Vec<u8>，reducer 完成 JSON
    解析、校验、结构复用和 digest 更新。
21. reuse_update 返回 Ok(false) 时推进成功 stat 并清除 error，但不更新 digest 或触发下游；
    返回 Err 时 S::Data 完全不变。
22. reducer 转移、stat 短路、last-known-good、subscription GC 和 Tokio paused-time debounce
    具有自动化测试。
23. UserIntent digest 的测试覆盖 empty、package 内容变化、顺序变化以及相同子 digest 序列
    的稳定性。

## 与 RFC 0001 的关系

RFC 0001 仍然是 package spec、revision、installed identity 和 marker 的历史验证。RFC 0002
不要求保留其一次性 action pipeline 作为长期 daemon 架构。

后续安装状态机可以复用 RFC 0001 中仍然成立的 PackageSpec 约束，但必须以本 RFC 的
Merged Intent 作为持续变化的目标输入。

RFC 0001 的 `<store>/installed` 验证布局不构成本 RFC 的长期承诺。本 RFC 使用
`IMST_DATA/{dl,pkgs,tmp}` 作为后续设计起点。

## 缺点

- 泛型 FileSpec/FileEvent/FileEffect 增加 Rust 类型复杂度。
- digest 需要稳定的 canonical encoding，不能直接 hash JSON 原始文件。
- unbounded event channel 初期缺少 backpressure。
- invalid 配置会保持旧目标，管理员必须通过状态观察发现错误。
- subscription GC 只回收观察和 intent contribution，不回收 package 数据。
- 本 RFC 没有解决 daemon crash 后内存 state 的恢复。

## 理由与替代方案

### 为什么使用 FileSpec marker

`FileEvent<TopConfigSpec>` 和 `FileEvent<IntentConfigSpec>` 直接表达领域角色，同时允许复用
相同加载协议。它比 `FileEvent<()>` 或 `FileEvent<Path>` 更易读，也避免给通用基础类型实现
领域 trait。

### 为什么顶层仍使用 AnyEvent/AnyEffect

系统能力集合应保持封闭、可穷尽匹配。泛型只复用文件加载机械部分，不隐藏 TopConfig 与
UserIntent 不同的下游行为。

### 为什么失败保留 last-known-good

外部文件可能处于写入中、暂时不可读或包含错误。把失败解释为空目标会错误触发
subscription GC 或未来 package prune。失败只更新诊断信息，修复后由持续 reload 自愈。

### 为什么 completion 后续订 fallback reload

inotify 可能丢失通知。未 invalidated completion 后续订一个 30 秒 reload，形成低频兜底
检查；工作期间已经收到 reload request 时，invalidated completion 改用 1 秒，加快追赶。
stat 未变化时 handler 会短路，因此稳定文件只产生 stat 开销，不重复读取和解析。正常
inotify 通知使用同一个 key 和 5 秒 timeout，可以替换 30 秒 fallback deadline 并及时
触发。

### 为什么 key 使用 String

key 只表达 debounce 等价分组，真正领域类型由被包装的 AnyEvent 表达。内部 namespaced
String 足够，并允许未来 shape 复用同一 service。

## 未决问题

- JSON 未知字段应拒绝还是忽略？
- 未来是否需要支持 JSON 之外的配置格式？
- canonical encoding 的长期版本化协议是什么？
- Stat 需要包含 mtime 之外的哪些字段？
- invalidated completion 的 1 秒、inotify 的 5 秒和 fallback 的 30 秒 timeout 是否需要
  通过 bootstrap 配置？
- effect task panic 或取消时，如何避免 stage 永久停留在 Submitted/Working？
- subscription path 移除时，Submitted/Working handler 的具体收尾策略是什么？
- daemon 优雅退出和 service 重启策略是什么？

## 未来可能性

- 基于 UID、用户组和 HOME 扫描产生额外 subscription roots。
- IPC 查询、状态订阅和管理接口。
- Merged Intent 到 InstalledSet 的 realize/prune 状态机。
- 下载缓存和 package object 的独立 GC。
- effect backpressure 和更完整的 runtime supervision。
