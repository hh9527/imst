# imst

`imst` 是 **immutable shared toolstore** 的缩写。

这是一个仍处于孵化阶段的实验项目，目标是为 `mise` 这类用户态包管理器和工具版本管理器提供一个共享的、不可变的底层存储层。

`imst` 不是包管理器。它不负责版本求解、工具激活、修改 shell 环境、安装 command shim，也不管理用户 profile。它的职责更窄：根据受限的 package spec 获取或准备输入、完成校验、物化到共享 store，并暴露一个 ready 的不可变结果。

项目的核心动机是减少同一台机器上多个用户之间重复下载、重复缓存和重复展开的工具分发物。

## 状态

项目仍处于早期实验阶段。

第一个可运行 RFC 聚焦一个很小的验证闭环：

- 解析 package spec
- 计算确定性的 revision
- 模拟下载和安装 action
- 写入 installed marker
- 在重复运行时跳过已经 ready 的结果

当前设计目标见 [RFC 0001](rfc/0001-package-spec-to-installed-marker.md)。

## 文档

- [VISION.md](VISION.md)：项目动机、边界和长期图景
- [rfc/0001-package-spec-to-installed-marker.md](rfc/0001-package-spec-to-installed-marker.md)：第一个可运行验证 RFC

## 试运行

```sh
cargo run -- --requests examples/request.json --store /tmp/imst-store
```

默认以 dry-run 模式运行。它会写入用于验证 store 流程的 marker 和 cache 文件，但还不会执行真实下载或 archive 展开。
