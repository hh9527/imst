# RFC 0002: Reducer/Effect 与请求文件持续调谐

## 摘要

本 RFC 提议建立 `imst daemon` 的第一套持续调谐执行模型，并用用户
`requests.json` 的观察、重载和 intent 更新验证这套模型。

RFC 0001 把安装描述为一次由入口驱动的流程。本 RFC 改用持续变化的目标和持续变化的
环境作为输入：daemon 接收 event，以同步 reducer 修改内存 state 并声明 effect；effect
异步访问外部环境，再把观察结果作为新的 event 投回 daemon。

```text
Event
  -> reduce(State)
  -> Effects
  -> async apply(Ctx)
  -> EventEmitter
  -> Event
```

本 RFC 不设计完整的安装、prune 或 GC 状态机。它先完成一个较小但完整的纵向切片：

- 持续观察用户 `requests.json` 的文件状态。
- 在文件变化时避免重复地并发读取。
- 保存最后一次成功解析和规整化的数据。
- 通过确定性的 `data_rev` 判断部署目标是否发生语义变化。
- 对 intent 更新进行全局 debounce。
- 以固定 service 和有限 effect 两类异步任务组成 daemon runtime。

如果这个模式得到验证，后续 RFC 可以把它继续应用到 `RootConfig`、`WatchList`、
`DesiredGoals` 和 `InstalledSet` 等更多 state shape。

## 动机

`imst` 的安装不应由一次性的 install command 驱动。管理员配置、系统用户集合、用户自己
拥有的 `requests.json`、文件系统状态以及 store 内容都会持续变化。daemon 的职责是持续
观察这些变化，并使自己有权管理的状态向最新目标收敛。

长期图景可以简化为：

```text
RootConfig
    -> WatchList
    -> DesiredGoals
    -> InstalledSet
```

这里的箭头不是一次性转换。每一层都可能继续变化，每一条边都需要反复调谐。安装和
prune 最终会同时出现在 `DesiredGoals -> InstalledSet` 的调谐中。

用户的部署目标来自各用户自己拥有的配置文件。daemon 可以读取和合并这些配置，但没有
权力修改它们。因此，用户配置不是 daemon 可以消费并删除的任务队列；它是持续存在、
持续变化的目标来源。

RFC 0001 的同步 action pipeline 足以验证 marker，但不适合直接承担以下行为：

- 文件 watcher 随时产生新观察。
- 一个外部操作执行期间，其依据的输入可能再次变化。
- 相同变化不应不断产生重复工作。
- effect 完成后可能需要立即接续下一次 effect。
- 多次快速变化应合并为一次下游 intent 更新。

因此，本 RFC 先固定 reducer/effect 模式和第一个文件调谐状态机，不急于设计完整动态 DAG
或抽象化的状态机框架。

## 指南级说明

daemon runtime 持有唯一的内存 `State`。所有状态修改都通过 `AnyEvent` 的 reducer 完成。
reducer 不直接访问文件系统、timer 或其他外部环境；它只能修改 state，并向 `Effects`
中加入具体的 `AnyEffect`。

effect 是有限的异步任务。它通过 `Ctx` 使用外部能力，并通过 `EventEmitter` 投递新的
event。effect 不能直接访问或修改 `State`。

长期运行的 file watcher 和 update-intent debouncer 不是 effect。它们是由 daemon 顶层
任务创建和监督的固定 service。effect 可以通过 `Ctx` 中的 sender 预触发这些 service，
service 则通过 `EventEmitter` 产生 event。

对于一个被观察的 `requests.json`，daemon 维护：

```text
FileState {
    path,
    stat,
    ok,
    error,
    updating
}
```

当 watcher 发现 stat 变化时，它投递 `StatChanged`。如果新 stat 表示普通文件，并且当前
没有 reload 在途，reducer 设置 `updating = true` 并声明 `Reload` effect。

`Reload` 完成后必须投递一个 `FileUpdated`。成功结果替换最后一次成功数据并清除错误；
失败结果保存错误，但不清除最后一次成功数据。effect 操作后的 stat 如果表明还需要继续
reload，reducer 直接声明下一个 `Reload` 并保持 `updating = true`；否则设置
`updating = false`。

成功数据包含规整化后的 `RequestSet` 和确定性摘要 `data_rev`。只有 `data_rev` 发生变化
时，reducer 才声明无 payload 的 `UpdateIntent` effect。

`UpdateIntent` effect 不直接启动一个独立 timer。它只通过 `Ctx` 中的 sender 预触发固定
的 update-intent service。该 service 持有唯一可重置 timer。安静期结束后，service 投递
一个无 payload 的 `UpdateIntent` event。该 event 从当时最新的 `State` 重新计算合并后的
intent，而不使用 effect 创建时的旧数据快照。

## 参考级说明

### 具体类型与分层

本 RFC 不引入开放式 `Event` 或 `Effect` trait。event 和 effect 是 `imst` 内置、封闭、
可审计的能力集合，使用具体枚举表达。

顶层枚举采用 wrapper-style variant：

```text
AnyEvent =
    File(FileEvent)
  | Intent(IntentEvent)

AnyEffect =
    File(FileEffect)
  | Intent(IntentEffect)
```

第二层领域枚举采用 named-field variant：

```text
FileEvent =
    StatChanged {
        path: Path,
        stat: FileStat,
    }
  | FileUpdated {
        path: Path,
        stat: FileStat,
        result: Result<FileOk, FileUpdateError>,
    }

FileEffect =
    Reload {
        path: Path,
    }

IntentEvent =
    UpdateIntent {}

IntentEffect =
    UpdateIntent {}
```

实际实现可以为第二层类型提供 `From` 实现，以减少包装噪声，但 reducer 和 effect executor
最终只需要处理 `AnyEvent` 和 `AnyEffect` 两个封闭集合。

本 RFC 刻意不提取通用 reducer/effect 框架。后续 shape 应先沿用具体类型模式，只有在出现
真实、稳定的重复后才考虑抽象。

### Reducer

`AnyEvent` 提供同步 reducer：

```text
AnyEvent.reduce(
    self,
    state: &mut State,
    effects: &mut Effects,
)
```

其中：

- `State` 是 daemon 的 reducer-owned 内存状态。
- `Effects` 是本次 reduce 新声明的有限 effect 集合。
- reducer 必须同步完成。
- reducer 不执行文件 IO、等待 timer 或访问网络。
- reducer 是修改 `State` 的唯一入口。
- event 没有返回值。

IPC/RPC 不属于本 RFC。未来如果请求响应语义需要编译期保证，可以单独增加
`ReduceWithReply`，而不改变本 RFC 的普通 event 模型。

### Effect 与 EventEmitter

`AnyEffect` 提供异步执行入口：

```text
async AnyEffect.apply(
    self,
    emitter: EventEmitter,
    ctx: &Ctx,
)
```

`EventEmitter` 提供简单的事件投递能力：

```text
EventEmitter.emit(event: AnyEvent)
```

初始实现可以使用 Tokio unbounded MPSC channel。`EventEmitter` 持有可 clone 的 sender，
daemon 顶层任务持有唯一 receiver。

effect 可以产生零个、一个或多个 event。业务失败也必须通过 event 返回 reducer，不通过
effect 直接修改状态。effect task 的 panic 或取消属于 runtime 故障，不等同于业务失败。

`Ctx` 提供 effect 所需的外部能力和固定 service 的触发入口。它不保存领域真相，也不能
向 effect 暴露可变 `State`。

### State shape

本 RFC 只固定验证文件调谐所需的 state：

```text
State {
    files: Map<Path, FileState>,
    intent: IntentState,
}

FileState {
    path: Path,
    stat: FileStat,
    updating: bool,
    ok: Option<FileOk>,
    error: Option<FileUpdateError>,
}

FileStat {
    mtime: SystemTime,
    ty: FileType,
}

FileOk {
    data: RequestSet,
    rev: DataRev,
}
```

`FileType` 至少能够区分普通文件和其他文件类型：

```text
FileType = RegularFile | Directory | Symlink | Other
```

`stat` 是最近一次被 reducer 接受的路径观察。WatchList 如何首次创建 `FileState`、路径
不存在如何表达、stat 失败如何表达，不在本 RFC 中最终固定；实现验证可以由初始化代码
创建 state，并只投递成功的 stat observation。

`ok` 是最后一次成功读取、解析和规整化的数据，不保证对应当前 `stat`。更新失败时必须
保留 `ok`，使暂时的读取或解析错误不会立即撤销上一次有效部署目标。

`error` 是最后一次 reload 失败。reload 成功时必须清除它。错误的长期分类、展示和重试
策略不在本 RFC 中固定。

`updating` 表示当前存在负责使文件数据追赶最新 stat 的 reload 工作。它不是某个 task 的
持久化 ID。本 RFC 依靠每个 `FileState` 同时最多一个 reload effect 的约束使用 boolean；
如果后续需要处理删除后重建或更复杂的过时 completion，可以扩展为 operation ID。

### DataRev

`FileOk.data` 是解析、校验并规整化后的领域对象，不是文件原始字节。

```text
data_rev = deterministic_hash(canonical_encoding(data))
```

本 RFC 要求：

- 相同规整化数据得到相同 `data_rev`。
- 仅 JSON 空白或其他无语义表示差异不改变 `data_rev`。
- `data` 和 `data_rev` 必须成对更新。
- `data_rev` 必须能够从 `data` 独立重算。
- `mtime` 不参与 `data_rev`。

本 RFC 可以沿用 RFC 0001 的紧凑 canonical JSON 与 SHA-256 作为验证算法，但不进一步固定
长期 canonical encoding。`RequestSet` 中哪些顺序具有语义，必须由其领域类型决定，不能
为了摘要稳定而任意重排具有行为含义的 item。

### StatChanged

`StatChanged` 表示 file watcher 得到一个新的路径观察：

```text
StatChanged {
    path,
    stat,
}
```

reducer 首先比较完整 `FileStat`，而不只比较 mtime。文件类型可能在 mtime 相同的情况下
发生变化。

状态迁移为：

| 条件 | 状态变化 | Effect |
| --- | --- | --- |
| `new_stat == state.stat` | 无 | 无 |
| stat 变化且不是普通文件 | 更新 `stat` | 无 |
| stat 变化、是普通文件且 `updating` | 更新 `stat` | 无 |
| stat 变化、是普通文件且 `!updating` | 更新 `stat`，设置 `updating = true` | `Reload` |

这里的 `updating` 用于避免 watcher 的重复 observation 产生并发 reload。reload 在途时仍要
接受更新的 stat，使当前 effect 完成后能够判断是否需要继续追赶。

### Reload effect

`Reload` 是有限 effect：

```text
Reload {
    path,
}
```

它负责：

1. 打开文件。
2. 从打开后的文件句柄获取 metadata。
3. 读取完整内容。
4. 解析并校验 `RequestSet`。
5. 把数据规整化并计算 `data_rev`。
6. 在操作完成后获取用于调谐的 `FileStat`。
7. 投递且只投递一个 `FileUpdated`。

打开文件会固定所引用的 inode，但不会冻结原地修改的内容。实现应至少比较读取前后的文件
metadata；如果读取期间文件版本发生变化，应返回失败结果或重新读取，不能把可能混合的
内容作为成功数据发布。

`Reload` 被成功提交后，runtime 必须保证 reducer 最终收到一个 `FileUpdated`。普通 IO、
解析或校验失败由 `FileUpdated(Err)` 表达。task panic、abort 或 daemon shutdown 的处理属于
runtime failure model；实现不能在 daemon 继续运行时让对应 `FileState.updating` 永久为
`true`。

### FileUpdated

`FileUpdated` 是 `Reload` 的 completion event：

```text
FileUpdated {
    path,
    stat,
    result,
}
```

它只允许在对应 `FileState.updating == true` 时发生。正常运行中在 `updating == false` 时
收到该事件属于非法迁移。

处理 `result`：

- `Ok(new_ok)`：比较旧、新 `data_rev`，以 `new_ok` 替换 `ok`，并清除 `error`。
- `Err(new_error)`：以 `new_error` 替换 `error`，并保留原有 `ok`。

处理 effect 操作后的 `stat`：

- reducer 先比较 completion 携带的 `stat` 与 state 中已经接受的当前 `stat`。
- 如果两者不同，并且 completion stat 是普通文件，则满足 reload 触发条件；reducer 更新
  `stat`，立即声明下一个 `Reload`，并保持 `updating = true`。
- 如果 stat 不满足 reload 触发条件，设置 `updating = false`，表达当前 effect 链已经完成。

`FileUpdated` 判断 reload 时不应用 `!updating` 条件，因为该事件必然发生在
`updating == true`，并且当前 effect 正在完成。此处的决定是让新 effect 接替当前 effect，
而不是与当前 effect 并发。

因此 completion 可以形成两种迁移：

```text
Updating -- FileUpdated, requires reload --> Updating
Updating -- FileUpdated, otherwise       --> Idle
```

不得先无条件设置 `updating = false` 再重新设置为 true。当前 effect 的完成与下一个 effect
的接替应作为一次 reducer 转换完成。

状态迁移为：

| Event | 条件 | 状态变化 | Effect |
| --- | --- | --- | --- |
| `FileUpdated(Ok)` | stat 需要继续 reload | 更新 stat/ok，清除 error，保持 updating | `Reload` |
| `FileUpdated(Err)` | stat 需要继续 reload | 更新 stat/error，保留 ok，保持 updating | `Reload` |
| `FileUpdated(Ok)` | stat 不需要继续 reload | 更新 stat/ok，清除 error，设置 updating=false | 无 |
| `FileUpdated(Err)` | stat 不需要继续 reload | 更新 stat/error，保留 ok，设置 updating=false | 无 |

`FileUpdated(Ok)` 还要比较旧、新 `data_rev`：

| 旧 revision | 新结果 | Intent effect |
| --- | --- | --- |
| `None` | `Ok(rev1)` | `UpdateIntent` |
| `rev1` | `Ok(rev1)` | 无 |
| `rev1` | `Ok(rev2)` | `UpdateIntent` |
| `rev1` | `Err` | 无 |
| `None` | `Err` | 无 |

同一个 completion 可以同时声明 `Reload` 和 `UpdateIntent`。这表示当前成功数据先向下游传播，
文件观察同时继续追赶更新版本。

### UpdateIntent debounce

`UpdateIntent` effect 和 event 都不包含 payload：

```text
IntentEffect::UpdateIntent {}
IntentEvent::UpdateIntent {}
```

effect 不携带 path、`data_rev` 或数据快照。它只通过 `Ctx` 中的 sender 向固定
update-intent service 发送一次预触发信号：

```text
UpdateIntentEffect.apply
    -> ctx.update_intent_tx.send(now)
```

service 持有 receiver 和唯一 timer，状态为：

```text
Idle
  -- trigger --> Armed(deadline)

Armed(deadline)
  -- trigger --> Armed(new_deadline)
  -- timer   --> emit UpdateIntentEvent -> Idle
```

Tokio 实现应使用一个长期 task、MPSC receiver 和 pinned `tokio::time::Sleep`。收到新 trigger
时，通过 `Sleep::reset(last_trigger + delay)` 重置 deadline。多个已经排队的 trigger 可以
合并，只保留最新触发时间。

timer 与 trigger 同时 ready 时，应优先处理 trigger，避免在新的变化刚到达时过早发出
event。实现可以使用带优先顺序的 `tokio::select!`。

timer 到期后，service 投递无 payload 的 `UpdateIntentEvent`，然后重新进入 Idle。已完成的
`Sleep` 不能直接在下一轮复用，否则它会持续立即 ready。

`UpdateIntentEvent` 从 reducer 当时持有的全部最新 `FileState.ok` 重新计算 intent。它不使用
触发 effect 时的数据，因此无需携带 revision，也不需要处理 effect payload 过时问题。

本 RFC 只要求 event 能够被正确 debounce 和投递。多用户 intent 的最终合并 shape、冲突
规则和 provenance 表达留给后续 RFC。

### Runtime 与任务监督

daemon 顶层任务持有两个 `JoinSet`：

```text
top
├── effects
└── services
    ├── update_intent
    └── file_watcher
```

顶层任务自己持有 event receiver、`State` 和 reducer loop。它每次取出一个 `AnyEvent`，同步
reduce，随后把本次声明的 `AnyEffect` 作为有限任务加入 effects `JoinSet`。

两类任务具有不同完成语义：

- effect task 是有限任务，正常完成是预期行为。业务失败应通过 event 返回。
- service task 是长期任务，daemon 正常运行期间不应自行返回。意外返回或 panic 必须由
  top 识别为 service failure，并决定终止或重启。

`file_watcher` service 持有文件系统 watcher，并把观察转换为 `StatChanged`。
`update_intent` service 持有 debounce receiver 和 timer，并产生 `UpdateIntentEvent`。

`Ctx` 至少向 effect 暴露：

```text
Ctx {
    update_intent_tx,
    file_watcher_tx,
    ... effect capabilities
}
```

sender 可以在内部使用并发安全的可变状态，但这些状态只属于 runtime mechanism，不属于
领域 `State`。

daemon shutdown 时，top 负责停止接受新工作、关闭 service 输入并等待或终止两个
`JoinSet`。完整优雅退出协议不在本 RFC 中固定。

## 不变量

实现必须维持：

1. 只有 reducer 可以修改 `State`。
2. effect 和 service 不能直接修改 `State`，只能投递 event。
3. reducer 不访问文件系统、timer、网络或其他外部环境。
4. 每个 `FileState` 同时最多存在一个 reload effect 链。
5. `updating == true` 表示 reload effect 链仍在负责追赶最新 stat。
6. `FileUpdated` 只能在 `updating == true` 时被接受。
7. reload 成功会原子地替换 `ok.data` 与 `ok.rev`，并清除 `error`。
8. reload 失败会更新 `error`，但不会清除最后一次成功的 `ok`。
9. `ok.rev` 必须能够由规整化后的 `ok.data` 确定性重算。
10. mtime 或 JSON 表示变化但 `data_rev` 不变时，不触发 intent 更新。
11. `data_rev` 变化时触发无 payload 的 `UpdateIntent` effect。
12. 所有 `UpdateIntent` effect 预触发同一个固定 debounce service。
13. 一个 debounce 安静期只产生一次无 payload 的 `UpdateIntentEvent`。
14. `UpdateIntentEvent` 始终基于 reducer 当时的最新 state 计算，而不基于旧 effect payload。

## 验收标准

本 RFC 的实现被认为完成，当且仅当满足：

1. runtime 使用具体的两层 `AnyEvent`/领域 event 和 `AnyEffect`/领域 effect 枚举。
2. top 持有独立的 effects 和 services `JoinSet`。
3. file watcher 和 update-intent debouncer 作为固定 service 运行。
4. 相同 `FileStat` 的重复 `StatChanged` 不产生 reload。
5. 非普通文件的 stat 变化不产生 reload。
6. 普通文件 stat 变化且未在更新时，产生一个 reload 并设置 `updating = true`。
7. reload 在途时的 stat 变化被 state 接受，但不会并发产生第二个 reload。
8. `FileUpdated` 只在 `updating == true` 时接受。
9. `FileUpdated` 发现仍需 reload 时，保持 updating 并直接接续下一个 reload。
10. `FileUpdated` 不需要继续 reload 时，设置 `updating = false`。
11. reload 成功保存规整化数据和确定性 `data_rev`，并清除旧错误。
12. reload 失败保存错误，同时保留最后一次成功数据。
13. 首次成功数据和变化后的 `data_rev` 产生 `UpdateIntent` effect。
14. 相同 `data_rev` 的重复成功读取不产生 `UpdateIntent` effect。
15. 连续预触发 update-intent service 会不断重置唯一 timer。
16. debounce 安静期结束后只投递一次无 payload 的 `UpdateIntentEvent`。
17. reducer 转移、reload 接续和 Tokio paused-time debounce 行为具有自动化测试。

## 与 RFC 0001 的关系

RFC 0001 仍然是 package spec、revision、installed identity 和 marker 的历史验证。RFC 0002
不要求保留其一次性 action pipeline 作为长期 daemon 架构。

本 RFC 不立即删除 RFC 0001 的实现，也不定义如何把已有安装 action 迁移成 event/effect。
后续安装状态机可以复用 RFC 0001 中仍然成立的领域约束，但应运行在本 RFC 建立的持续调谐
模型之上。

若两者在执行架构上冲突，以 RFC 0002 的 reducer/effect 与 daemon runtime 模型作为后续
设计方向；RFC 0001 的同步流程只视为早期验证入口。

## 缺点

- 本 RFC 只验证一个文件调谐 shape，尚不能完成多用户部署或共享安装。
- boolean `updating` 依赖每个文件最多一个 reload 链的约束，尚未提供通用 operation ID。
- unbounded event channel 缺少 backpressure。
- 固定 debounce service 增加了 runtime 生命周期管理。
- 保留最后成功数据意味着错误配置可能继续维持旧目标，需要后续状态观察接口清楚展示。
- 本 RFC 没有解决 daemon crash 后内存 state 的恢复。

## 理由与替代方案

### 为什么使用具体枚举而不是 trait

`imst` 的 event 和 effect 是封闭能力集合，不是插件接口。具体枚举能提供穷尽匹配、统一
日志、简单的异构队列和明确的审计边界，也避免 trait object、类型擦除和 object safety
问题。

### 为什么 effect 通过 EventEmitter 返回

外部操作可能产生零个、一个或多个观察结果，长期 service 也需要持续投递 event。
`EventEmitter` 让它们共享同一个单向反馈入口，并允许多个异步任务并发投递，而不暴露
`State`。

### 为什么不让每个 UpdateIntent effect 自己 sleep

每次变化各自创建 timer 只能过滤过时结果，不能形成真正的单一 trailing-edge debounce。
固定 service 持有唯一 timer；所有 effect 只发送 reset 信号，能明确保证安静期只产生一次
event，并避免持续创建和取消 timer task。

### 为什么保留最后一次成功数据

用户可能正在重写配置，文件也可能暂时不可读。一次瞬时错误不应自动等价为用户撤销全部
部署目标，尤其不能因此直接引发未来的 prune。保留 `ok` 可以让系统继续使用最后已知有效
目标，同时通过 `error` 暴露新版本的问题。

### 为什么本 RFC 不设计通用动态 DAG

当前只有一个得到充分讨论的 state shape。先以具体 reducer/effect 验证持续调谐、在途工作
和 debounce，能让后续抽象来自真实重复，而不是预先猜测完整 DAG 的节点和传播协议。

## 未决问题

- `FileState` 的初始、missing 和 stat-failed 状态最终如何表达？
- file watcher 如何接收动态 watch/unwatch 指令？
- `Reload` 如何精确定义读取期间文件版本稳定性的 metadata tuple？
- `FileUpdated` 的非法迁移在 production 中应 panic、忽略还是转化成 runtime error？
- effect task panic 或取消时，如何保证 `updating` 不永久停留为 true？
- update-intent debounce 的默认时长和配置入口是什么？
- `UpdateIntentEvent` 最终如何合并多用户数据并保留 provenance？
- daemon 的优雅退出和 service 重启策略是什么？

## 未来可能性

后续 RFC 可以沿用本 RFC 的模式继续设计：

- RootConfig 的持续观察和用户组解析。
- WatchList 的动态 watch/unwatch 调谐。
- 多用户 DesiredGoals 的合并与来源追踪。
- InstalledSet 的 realize、repair 和 prune 状态机。
- 下载缓存和 installed object 的独立生命周期。
- GC 策略和反向调谐。
- IPC 查询、状态订阅和 `ReduceWithReply`。
- runtime backpressure、effect cancellation 和 operation identity。
