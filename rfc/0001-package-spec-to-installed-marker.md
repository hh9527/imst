# RFC 0001: Package Spec 到 Installed Marker 的可运行验证

## 摘要

本 RFC 提议实现 `imst` 的第一个可运行验证闭环：从单用户全量意图中读取 package spec，计算公开、确定性的 `rev`，得到 installed identity，并执行一个 dry-run 安装流程，最终写入可重复检测的 installed marker。

这个版本不追求 MVP，也不追求功能完整。它只验证核心模型是否能跑起来：

```text
fn(spec) -> Installed
```

其中 `Installed` 在本 RFC 中先以 ready marker 的形式存在。重复运行时，`imst` 应能通过 marker 判断该 installed identity 已经 ready，并跳过重复安装流程。

## 动机

`imst` 的长期目标是在多用户环境下，为用户态包管理器提供共享、不可变、可复用的底层包存储机制。这个目标依赖几个核心判断：

- package spec 可以作为安装输入。
- `rev = hash(spec)` 可以作为公开、确定性的修订身份。
- `{ name, rev }` 可以识别一个 installed 结果。
- 一个 package spec 中的多个 items 共同组成一个安装单元。
- 只有整个 package spec 完成后，installed 结果才应被认为 ready。
- 已 ready 的 installed 结果应能被重复检测，从而避免重复执行。

在讨论 daemon、多用户权限、只读发布、GC 和上层工具集成之前，需要先验证这个最小安装骨架能否成立。

因此，本 RFC 的目标是实现一个可运行但刻意受限的验证版本。它不真实下载和展开内容，而是 dry-run 安装动作；但它必须真实生成 marker，因为 marker 是后续 installed 状态、重复检测和 ready 语义的基础。

## 指南级说明

使用者准备一个单用户全量意图。这个意图包含多个 package spec：

```json
{
  "packages": [
    {
      "name": "node",
      "items": [
        {
          "src": "https://example.invalid/node-v20.tar.gz",
          "digest": {
            "sha256": "aaa"
          },
          "dest": "",
          "kind": {
            "type": "Archive",
            "format": "TarGzip",
            "strip_components": 1
          }
        }
      ]
    },
    {
      "name": "node",
      "items": [
        {
          "src": "https://example.invalid/node-v22.tar.gz",
          "digest": {
            "sha256": "bbb"
          },
          "dest": "",
          "kind": {
            "type": "Archive",
            "format": "TarGzip",
            "strip_components": 1
          }
        }
      ]
    }
  ]
}
```

`imst` 读取这个意图后，对每个 package spec 执行同一套流程：

1. 读取 `name`。
2. 根据 package spec 计算 `rev`。
3. 得到 installed identity：`{ name, rev }`。
4. 检查对应 installed marker 是否已经存在且有效。
5. 如果 marker 已 ready，则跳过该 package spec。
6. 如果 marker 不存在，则 dry-run 显示会执行的安装动作。
7. dry-run 完成后，写入 ready marker。

同一个 `requests.json` 中允许存在多个相同 `name` 的 package spec。只要它们的 spec 不同，计算出的 `rev` 就不同，因此会得到不同的 installed identity，并可以同时存在。

本 RFC 不要求普通用户直接使用 `imst`。触发方式可以是 `main()`、测试代码或一个临时命令入口。重点是让模型能运行验证，而不是形成稳定用户界面。

## 参考级说明

### 核心数据类型

本 RFC 需要定义最小核心数据类型。

`RequestSet` 表示单用户全量意图：

```text
RequestSet {
    packages: Vec<PackageSpec>
}
```

`PackageSpec` 表示一个安装单元：

```text
PackageSpec {
    name: String,
    items: Vec<ItemSpec>
}
```

`name` 必须满足：

```text
[a-z][0-9a-z_\-.]+
```

并且不能以 `_`、`-` 或 `.` 结尾。也就是说，`name` 必须以小写字母开头，只能包含小写字母、数字、下划线、连字符和点号，并且最后一个字符必须是小写字母或数字。

此外，`_`、`-` 和 `.` 不能连续出现。例如 `foo--bar`、`foo_.bar`、`foo..bar` 都不合法。

`ItemSpec` 表示构成该 package spec 的一个输入：

```text
ItemSpec {
    src: Uri,
    digest: Digest,
    dest: RelativePath,
    kind: ItemKind
}
```

`src` 表示输入来源，采用 URL/URI 表达。RFC 0001 不真实获取 `src` 指向的内容，但实现应解析和保存它，并将其作为 package spec 的一部分参与 `rev` 计算。

RFC 0001 只支持完整的 `http://` 和 `https://` URI。不支持 scheme-relative URI，例如 `//example.com/a.tar.gz`；也不支持相对路径，例如 `./a.tar.gz` 或 `a.tar.gz`。

`Digest` 在本 RFC 中定义为：

```text
Digest {
    sha256: String
}
```

JSON 表达为：

```json
{
  "sha256": "..."
}
```

RFC 0001 暂时只支持 `sha256`。虽然本 RFC 不真实校验内容，但实现应解析 digest 结构，并将 digest 作为 package spec 的一部分参与 `rev` 计算。

`dest` 表示该 item 在 installed root 下的目标位置。installed root 由 `{ name, rev }` 决定；所有 item 的 `dest` 都相对于这个 installed root 解释。

例如 installed root 为：

```text
<store>/installed/<name>/<rev>
```

那么：

- `dest = ""` 对应 `<store>/installed/<name>/<rev>`。
- `dest = "abc/xyz"` 对应 `<store>/installed/<name>/<rev>/abc/xyz`。
- `dest = "/"` 不合法。
- 以 `/` 开头的绝对路径不合法。
- 包含 `.` 路径段不合法。
- 包含 `..` 路径段不合法。
- 试图逃出 installed root 的路径不合法。
- `dest` 必须已经是 normalized 之后的相对路径，例如 `a/./b` 不合法，应表达为 `a/b`。

`dest` 放在 `ItemSpec` 层，而不是 `kind` 层。`kind` 表示如何处理输入，`dest` 表示处理结果放到 installed root 下的哪里。

`ItemKind` 在本 RFC 中只需要表达有限能力集合：

```text
ItemKind =
    Archive { format: ArchiveFormat, strip_components: u32 }
  | BinaryFile
  | RegularFile
```

`ItemKind` 和 `ArchiveFormat` 是枚举类型，不是任意字符串。JSON 中的 `type` 和 `format` 是这些枚举 variant 的序列化表示，取值必须来自类型定义。

JSON 中 `kind` 使用对象形式表达，避免未来增加选项时改变结构风格：

```json
{
  "type": "Archive",
  "format": "TarGzip",
  "strip_components": 1
}
```

本 RFC 暂定支持的 `ArchiveFormat` 枚举：

```text
ArchiveFormat =
    TarGzip
```

不同 `kind` 对 `dest` 的约束不同：

- `Archive`：`dest` 表示展开目标目录；`dest = ""` 合法，表示展开到 installed root 顶层。未来真实展开时不原样信任归档权限，而是对权限做过滤和规范化：目录和可执行文件使用 `755`，普通文件使用 `644`，并去除 xattr。Archive 中的 symlink 应被忽略，不跟随、不创建，也不作为失败处理。
- `BinaryFile`：`dest` 表示目标文件路径；`dest` 不能为空；未来真实安装时应设置为 executable，权限语义为 `755`。
- `RegularFile`：`dest` 表示目标文件路径；`dest` 不能为空；未来真实安装时应设置为普通文件权限，权限语义为 `644`。

`imst` 只保证 package spec 被物化到共享位置上，不保证物化结果一定可运行。因此，如果某些包依赖 symlink 才能运行，忽略 symlink 后的可运行性问题应由上层包管理器或后续能力设计处理。

Archive 未来真实展开时，也应过滤 entry path：

- entry path 必须是 normalized relative path。
- entry path 不能是空路径。
- entry path 不能是 `/`。
- entry path 不能以 `/` 开头。
- entry path 不能包含 `.` 路径段。
- entry path 不能包含 `..` 路径段。
- entry path 不能逃出该 Archive item 的 `dest`。

不满足这些约束的 entry 应被忽略，并输出人类可读 warning。Archive 中不支持的 entry 类型也应被忽略并输出 warning。这个策略优先保证受限安装流程继续完成，但不承诺忽略后的结果一定可运行。

Archive 未来真实展开时只支持 regular file 和 directory。除此之外的文件类型，包括 symlink、hardlink、device、fifo、socket 等，都不支持，应被忽略并输出 warning。

本 RFC 不引入脚本、命令、post-install hook 或源码构建能力。

`InstalledIdentity` 表示安装结果身份：

```text
InstalledIdentity {
    name: String,
    rev: String
}
```

其中：

```text
rev = hash(spec)
```

`InstalledMarker` 表示 installed identity 已经 ready：

```text
InstalledMarker {
    installed_at: UtcDateTime
}
```

本 RFC 中 marker 暂时只固定 `installed_at` 一个字段。时间必须使用 UTC 时间。marker 不包含完整 spec、spec hash、item 摘要、`name`、`rev` 或 provenance 信息。

marker 的语义是：对应 installed identity 的 marker 存在、可解析，并包含合法的 UTC `installed_at`，则该 installed identity 被视为 ready。否则视为 not ready。

本 RFC 不持久化 `failed`、`in-progress` 或其他状态。dry-run 过程中的失败或中间状态只通过当次运行结果体现，不写入 installed marker。

### 验证路径布局

本 RFC 固定一个用于验证的 store 布局：

```text
<store>/installed/<name>/<rev>/.imst.json
```

其中：

- `<store>` 是运行验证时指定的 store 根目录。
- `<name>` 来自 `spec.name`。
- `<rev>` 来自 `sha256(canonical_json(spec))`。
- `.imst.json` 是 installed marker。

这个布局直接表达 installed identity 的二元结构 `{ name, rev }`。同一个 `name` 下可以存在多个不同 `rev`，对应多个不同 package spec。

本 RFC 不声明这是最终长期 store layout；它只是 0001 的可运行验证布局。

### Rev 计算

`rev` 必须由公开、确定性的算法计算得到。本 RFC 暂定：

```text
rev = sha256(canonical_json(spec))
```

本 RFC 不需要最终固定长期稳定算法，但实现中必须做到：

- 同一个 package spec 在重复运行中得到相同 `rev`。
- 不同 package spec 应得到不同 `rev`。
- `rev` 来自整个 package spec，而不是只来自 `items`。
- `spec.name` 参与 `rev` 计算；同样的 `items` 在不同 `name` 下会得到不同 `rev`。
- 计算 `rev` 时只关注类型中定义的字段。
- 输入中的未知字段会被忽略，不参与行为，也不参与 `rev` 计算。
- 编码时应按字段名顺序输出。
- 编码应使用紧凑 JSON，不引入无意义空白。

后续 RFC 可以进一步严格定义 canonical encoding 和版本化策略。

### Dry-run 安装流程

本 RFC 的安装流程可以表示为：

```text
for spec in request_set.packages:
    identity = identity_of(spec)

    if marker_ready(identity):
        skip(identity)
        continue

    for item in spec.items:
        dry_run(item)

    write_ready_marker(identity)
```

`dry_run(item)` 不执行真实下载、校验或展开。它只需要记录或显示将要执行的动作，例如：

- 将从 `src` 获取内容。
- 将用 `digest` 校验内容。
- 将根据 `kind` 处理内容。

dry-run 输出只面向人类阅读，用于调试和验证流程。本 RFC 不定义机器可读事件格式，也不要求输出格式长期稳定。测试不应依赖 dry-run 日志文本。

### 内置行为接口

本 RFC 应把安装过程中的内置行为接口化。每一种受支持的行为都可以表示为一个 action，并通过统一入口执行：

```text
Action.apply(ctx) -> Result<(), ActionError>
```

例如：

```text
UnpackArchive { ... }.apply(ctx)
InstallMarker { ... }.apply(ctx)
```

其中 `ctx` 表示执行上下文。它包含 action 执行所需的环境信息，例如 store 根位置、时间来源、dry-run 开关等。`ctx` 将来可以来自 `imst` 配置文件；在 RFC 0001 中可以固定为测试或 `main()` 构造的上下文。

这个接口化不是为了引入开放式插件系统，也不是允许用户在 request 中表达任意行为。它只用于组织 `imst` 内置、受限、可预测的行为集合。

这样做有几个目的：

- mock 测试：测试可以替换时间、日志、文件系统或 dry-run 执行效果。
- 日志输出：每个 action 可以明确描述自己将要执行或已经执行的动作。
- 后续扩展：未来增加真实下载、真实展开、权限收敛等能力时，可以沿用同一执行模型。
- 能力约束：所有 action 仍然必须属于 `imst` 定义的有限能力集合，不能扩展成任意脚本执行。

### Action 失败语义

即使本 RFC 中大部分 action 可以是 dry-run，也需要定义失败语义。

`Action.apply(ctx)` 返回 `Result<(), ActionError>`。对一个 package spec 来说，任意 action 失败，都表示该 package spec 本次安装失败。失败时：

- 不写 installed marker。
- 不持久化 `failed` 状态。
- 不持久化 `in-progress` 状态。
- 失败信息只通过当次运行的人类可读日志体现。
- 已经存在的合法 marker 不受影响。

`InstallMarker` 应是当前 package spec 的最后一个 action。只有前面的 item action 全部成功后，才允许执行 `InstallMarker` 并写入 marker。

因此，marker 仍然保持单一语义：合法 marker 存在即 ready；不存在或非法即 not ready。

`InstallMarker` 也必须通过同一个 `Action.apply(ctx) -> Result<(), ActionError>` 接口执行。它不是安装流程之外的特殊写文件步骤，而是 action pipeline 的最后一步。

### 执行上下文

RFC 0001 中的 `ctx` 可以由测试或 `main()` 固定构造。它至少需要包含：

- store root：用于决定 installed marker 的写入位置。
- time provider：用于生成 UTC `installed_at`，并允许测试注入固定时间。
- dry-run flag：用于指示 item action 只显示动作、不执行真实下载或展开。

日志不要求作为 `ctx` 的一部分。它可以由全局日志设施或运行时环境提供。`ctx` 只承载 action 执行所必需、且需要显式传入或测试替换的信息。

后续 RFC 可以让 `ctx` 来源于 `imst` 配置文件，或扩展更多运行时能力。本 RFC 只要求这些能力足以支撑 dry-run action、marker 写入和可测试性。

### Ready 语义

ready 是 package spec 级别的状态，而不是 item 级别的状态。

一个 package spec 包含多个 items 时，只有所有 items 的 dry-run 流程都完成后，才能写入 installed marker。marker 一旦 ready，就表示该 installed identity 对应的 package spec 已经整体完成。

### 重复执行

重复执行时，`imst` 必须先检查 installed marker。

如果 marker 表示 `{ name, rev }` 已 ready，则跳过该 package spec 的 dry-run 安装流程。这个行为用于验证 installed marker 可以作为重复检测的依据。

## 缺点

本 RFC 有意不验证很多关键能力：

- 不真实下载内容。
- 不真实校验 digest。
- 不真实展开 archive。
- 不验证只读权限收敛。
- 不验证 daemon 作为唯一供应方。
- 不验证多用户 root set。
- 不验证 GC。
- 不验证和上层包管理器的真实集成。

因此，本 RFC 不能证明 `imst` 已经具备可用的共享 store 能力。它只能证明 package spec、rev、installed identity、ready marker 和重复检测这条最小链路可以运行。

## 验收标准

本 RFC 的实现被认为完成，当且仅当满足以下条件：

1. 可以解析一个单用户全量意图，其中包含一个或多个 package spec。
2. 可以支持同一个全量意图中存在多个相同 `name` 的 package spec。
3. 可以为每个 package spec 计算稳定的 `rev = sha256(canonical_json(spec))`。
4. 相同 package spec 在重复运行中得到相同 `rev`；不同 package spec 得到不同 `rev`。
5. 可以为每个 package spec 得到 installed identity：`{ name, rev }`。
6. 可以将 installed identity 映射到 RFC 0001 的验证布局：`<store>/installed/<name>/<rev>/.imst.json`。
7. 第一次运行时，如果 marker 不存在，程序会执行 dry-run action，并写入合法 marker。
8. marker 内容只包含合法 UTC `installed_at`。
9. 第二次运行同一个全量意图时，如果 marker 已存在且合法，程序会跳过对应 package spec。
10. 一个 package spec 包含多个 items 时，只有该 spec 的所有 item action 都完成后，才写入 marker。
11. 安装流程中的内置行为通过 `Action.apply(ctx)` 这类统一接口执行。
12. 如果任意 item action 失败，程序不得写入 marker；下一次运行仍会重新尝试该 package spec。
13. `InstallMarker` 也通过 `Action.apply(ctx)` 执行，并且是当前 package spec 的最后一个 action。
14. `ctx` 至少包含 store root、time provider 和 dry-run flag。

本 RFC 不要求真实下载、真实展开、真实权限收敛、多用户、daemon、GC 或上层工具集成。测试不应以这些能力作为通过条件。

## 理由与替代方案

### 为什么不先做 daemon

daemon 是最终图景中的核心组件，但第一版直接引入 daemon 会过早涉及进程生命周期、权限、发现机制和状态同步。本 RFC 先验证 daemon 未来要执行的核心安装骨架。

### 为什么不先做真实下载和展开

真实下载和展开会引入网络、归档格式、安全校验和文件系统边界。它们很重要，但不是第一步最需要验证的内容。第一步更需要确认 installed identity 和 ready marker 是否能支撑后续流程。

### 为什么 marker 必须真实写入

如果只打印 dry-run 计划，就无法验证重复执行和 ready 检测。真实 marker 是本 RFC 最小但必要的持久化状态。

### 为什么允许同名 package spec 并存

`name` 表示上层意图中的包名，不是唯一身份。同名不同版本、不同来源或不同 digest 都应能并存。`rev` 负责区分不同的 package spec。由于 `rev = hash(spec)`，`spec.name` 也参与 `rev` 计算；同样的 `items` 在不同 `name` 下会得到不同 `rev`，这是符合预期的。

## 先例

本 RFC 的模型受到以下系统或模式启发：

- Nix store 中通过不可变 store path 表达已实现结果。
- Bazel 等构建系统中通过输入集合决定可复用结果。
- 用户态工具版本管理器中，同一工具名可能对应多个不同版本和平台分发物。

本 RFC 不直接复刻这些系统，只借鉴其“输入决定结果身份”和“已完成结果可复用”的思想。

## 未决问题

- `canonical_json(spec)` 的长期编码细节和版本化策略应如何定义？
- marker 后续是否需要扩展 provenance、schema version 或更丰富状态信息？
- 0001 之后是否继续沿用 `<store>/installed/<name>/<rev>/.imst.json` 作为长期布局？
- 后续是否需要持久化 `failed`、`in-progress` 或其他非 ready 状态？
- 后续是否需要定义机器可读 status 或 event 模型？

## 未来可能性

后续 RFC 可以在本 RFC 的基础上继续推进：

- 真实下载与 digest 校验。
- Archive 展开和文件放置。
- 只读权限收敛与发布语义。
- daemon 作为唯一供应方。
- 多用户信任连接。
- root set 与 GC。
- 与上层包管理器的真实集成。
