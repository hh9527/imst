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
- 如何合并频繁文件通知，并在真实加载完成后补一次 force-rescan。
- 如何由顶层配置的 subscription set 驱动 intent-source 的发现和回收。

## 指南级说明

daemon 启动时读取环境变量：

```text
IMST_CONFIG=/etc/imst/config.json
IMST_DATA=/opt/imst/data
```

`IMST_CONFIG` 可以指向 JSON 或 TOML 文件。未设置时默认使用 `/etc/imst/config.{json|toml}`；
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
    "/path/to/user2/intent.toml"
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

两种文件均可使用 JSON 或 TOML，格式由文件扩展名决定。文件先反序列化成类型化数据，再
规整化并计算确定性 revision。输入格式、空白和无语义顺序不直接参与 revision。

file watcher 始终观察顶层配置路径，并动态观察当前 subscription set。文件通知不执行
stat 或读取，只通过 keyed debounce 投递类型化 `ReloadRequested` event。

`ReloadRequested` 从 `FileState` 快照 `prev_stat` 并声明 reload effect。handler 开始时投递
`ReloadStarted`，随后执行 stat：

```text
current_stat == prev_stat
    -> 短路内容读取
    -> ReloadFinished { data: None, error: None }

current_stat != prev_stat
    -> 读取、解析、规整化并计算 revision
    -> ReloadFinished { data: Some(...) }

加载失败
    -> ReloadFinished { error: Some(...) }
```

失败只更新错误观察，不替换 last-known-good stat/data。首次启动时 last-known-good 是合法
empty 数据；因此顶层配置或单个用户意图文件出错都不会使 daemon OOS。文件修复后，持续
reload 会自动清除错误并推进有效数据。

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
}

UserIntentData {
    packages: Vec<PackageSpec>,
}

Versioned<T> {
    data: T,
    rev: String,
}
```

`TopConfigData.subscribe` 的规整化至少包括：

- path 必须是 UTF-8 absolute path。
- path 必须 normalized，不包含 `.` 或 `..` segment。
- 重复 path 去重。
- 集合使用确定性顺序编码。

`UserIntentData.packages` 保留具有领域语义的顺序。PackageSpec 的具体校验规则可以复用
RFC 0001，但输入容器改为结构化 JSON/TOML 文件。

两种 empty last-known-good 为：

```text
TopConfigData { subscribe: {} }
UserIntentData { packages: [] }
```

empty 数据具有确定性 revision，是正常有效状态，不是错误或 OOS 状态。

### JSON 与 TOML

格式由扩展名决定：

```text
.json -> JSON
.toml -> TOML
其他  -> UnsupportedFormat error
```

revision 来自规整化后的类型化数据：

```text
rev = hash(canonical_encoding(normalized_data))
```

同一语义数据使用 JSON 或 TOML 表达时必须得到相同 revision。实现可以使用统一 canonical
JSON 或其他内部确定性编码；不能直接 hash 原始文件字节。

未知字段、字段顺序和具体 canonical encoding 的长期兼容策略留给后续 RFC。本 RFC 的实现
必须至少保证同一版本程序内结果确定。

### FileSpec

两类文件通过 marker spec 关联 key、data 和规整化规则：

```text
FileSpec {
    type Key
    type Data

    normalize(data: Data) -> Result<Data, FileLoadError>
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
    type Data: Default
        + Serialize
        + DeserializeOwned
        + Send
        + 'static;

    fn normalize(
        data: Self::Data,
    ) -> Result<Self::Data, FileLoadError>;
}

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
    value: Versioned<S::Data>,
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

`stage` 语义：

- `Idle`：没有 loader 工作。
- `Submitted`：reload effect 已提交，但 handler 尚未开始。
- `Working`：handler 已经开始执行。

本 RFC 保留这三个 variant，不设计取消、Retiring 或 operation identity。

`invalidated` 表示 Submitted/Working 期间又到达一个 `ReloadRequested`。该请求必须重新
进入 debounce 队列，所以 `invalidated == true` 同时保证已经存在后续 pending request。

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
        data: Option<Versioned<S::Data>>,
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
| Submitted | invalidated=true | 重新 debounce 同一个 ReloadRequested |
| Working | invalidated=true | 重新 debounce 同一个 ReloadRequested |

`ReloadStarted` 只允许在 Submitted，迁移到 Working。

handler 必须先投递 `ReloadStarted`，再执行 stat 和可选读取，最后恰好投递一个
`ReloadFinished`。同一个 producer 的事件顺序必须保持。

`ReloadFinished` 合法组合：

| stat | error | data | 语义 |
| --- | --- | --- | --- |
| 与 prev_stat 相同 | None | None | stat 短路，未读取 |
| None | Some | None | stat 失败 |
| Some | None | Some | 加载成功 |
| Some | Some | None | stat 后的类型、读取、解析或校验失败 |

以下组合非法：

- data 和 error 同时为 Some。
- stat 为 None 但 data 为 Some。
- stat、error、data 同时为 None。

completion 只允许在 Working，完成后进入 Idle 并清除 invalidated。

结果应用：

- `data=Some`：推进 last-known-good stat/value，清除 error。
- `error=Some`：保留 last-known-good stat/value，只替换 error observation。
- short-circuit：不修改 stat/value/error。

force-rescan：

- completion 未 invalidated 且不是 short-circuit 时，debounce 一个 ReloadRequested。
- completion 已 invalidated 时不重复投递，因为 busy request 已保证 pending reload 存在。
- short-circuit 不主动 force-rescan，避免稳定文件形成无限循环。

### 顶层配置调谐

file watcher 固定观察 `BootstrapConfig.config_path`。启动和文件通知都使用：

```text
emit_debounce(
    TopConfig::ReloadRequested { key: () },
    config_reload_timeout,
    "config:reload",
)
```

TopConfig 加载失败只更新 error，当前 subscription roots 不变。

TopConfig 加载成功且 revision 变化时，reducer 用新 subscribe set 替换 WatchList，并声明
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

用户意图加载成功且 revision 变化时，reducer debounce 无 payload `UpdateIntentEvent`：

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
    sources: Map<Path, Versioned<UserIntentData>>,
    rev: String,
}
```

本 RFC 先按 source 保存合并视图，不解决不同文件中同名 package 的冲突、依赖或版本求解。
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
8. Reload effect 必须携带 reducer 快照的 prev_stat。
9. 文件失败不能替换 last-known-good stat/value。
10. TopConfig 失败不能撤销 subscription roots。
11. UserIntent 失败不能撤销该 source 的最后有效贡献。
12. 只有成功 TopConfig 数据可以改变 WatchList。
13. Merged Intent 只能包含当前 WatchList 中的 source。
14. 同 key debounce 只投递安静期内最后一个 event。
15. short-circuit completion 不触发 force-rescan。
16. 非 short-circuit completion 必须确保一次后续 reload：自行 force-rescan，或复用已经
    pending 的 invalidated request。

## 验收标准

1. 支持从环境变量构造不可变 BootstrapConfig，并提供约定默认值。
2. 初始化并独占管理 IMST_DATA/{dl,pkgs,tmp}。
3. JSON 和 TOML TopConfig 能解析为相同类型化数据和 revision。
4. JSON 和 TOML UserIntent 能解析为相同类型化数据和 revision。
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
16. 非短路 completion 补一次 force-rescan，短路 completion 正常结束。
17. reducer 转移、stat 短路、last-known-good、subscription GC 和 Tokio paused-time debounce
    具有自动化测试。

## 与 RFC 0001 的关系

RFC 0001 仍然是 package spec、revision、installed identity 和 marker 的历史验证。RFC 0002
不要求保留其一次性 action pipeline 作为长期 daemon 架构。

后续安装状态机可以复用 RFC 0001 中仍然成立的 PackageSpec 约束，但必须以本 RFC 的
Merged Intent 作为持续变化的目标输入。

RFC 0001 的 `<store>/installed` 验证布局不构成本 RFC 的长期承诺。本 RFC 使用
`IMST_DATA/{dl,pkgs,tmp}` 作为后续设计起点。

## 缺点

- 泛型 FileSpec/FileEvent/FileEffect 增加 Rust 类型复杂度。
- JSON/TOML 双格式需要统一 canonical revision，不能直接 hash 原始文件。
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

### 为什么 completion 后 force-rescan

真实加载期间文件可能再次变化。完成后补一个 debounced reload，可以替代 file watcher 的
专用 ForceRescan。stat 未变化时下一次 handler 会短路，因此不会形成持续读取循环。

### 为什么 key 使用 String

key 只表达 debounce 等价分组，真正领域类型由被包装的 AnyEvent 表达。内部 namespaced
String 足够，并允许未来 shape 复用同一 service。

## 未决问题

- `/etc/imst/config.{json|toml}` 未显式设置时如何选择；两者同时存在是否报错？
- JSON/TOML 未知字段应拒绝还是忽略？
- canonical encoding 的长期版本化协议是什么？
- Stat 需要包含 mtime 之外的哪些字段？
- FileWatch 丢失通知时，周期性全量 rescan 如何实现？
- effect task panic 或取消时，如何避免 stage 永久停留在 Submitted/Working？
- subscription path 移除时，Submitted/Working handler 的具体收尾策略是什么？
- daemon 优雅退出和 service 重启策略是什么？

## 未来可能性

- 基于 UID、用户组和 HOME 扫描产生额外 subscription roots。
- IPC 查询、状态订阅和管理接口。
- Merged Intent 到 InstalledSet 的 realize/prune 状态机。
- 下载缓存和 package object 的独立 GC。
- effect backpressure 和更完整的 runtime supervision。
