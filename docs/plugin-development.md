# 插件开发

PostWarden 使用构建期 Rust 插件。插件实现 `postwarden_plugin_api::Plugin`，在服务启动时注册；新增或升级插件需要重新构建服务。插件代码与服务同进程运行，应按受信任代码审核。

## 插件契约

1. 在 `src/plugins/<name>.rs` 实现插件，例如 `src/plugins/example_policy.rs`。该文件是内置插件的维护边界，放置插件自身的钩子、配置、解析和业务逻辑。实现稳定的 `id()` 和用于解释持久化暂停状态的 `version()`。维护现有插件时只改对应实现文件；通用接口和宿主基础设施由平台维护。
2. 按需实现 `compose`、`before_submit`、`before_send`、`after_result`。`compose` 的 `input: &serde_json::Value` 由插件自行解析和校验；它只能返回修改后的草稿或拒绝。核心会再次校验草稿和发件身份授权。
3. 用 `default_config()` 定义尚未保存配置时的运行值，用 `config_template()` 提供首次编辑时展示的安全示例，并用 `validate_config()` 在管理员保存配置前检查结构和业务规则。配置作为 `PluginContext.config` 传给钩子；核心加密保存配置，管理 API 不回显已保存的值。
4. 用 `PluginContext.storage` 保存插件私有 JSON 数据。`get`、`put`、`delete` 按插件 ID 隔离，值在数据库中加密。插件自行选择键名和 JSON 版本结构。存储接口由公共 API crate 提供，宿主适配、通用存储实现及数据库差异位于 `src/plugin_runtime/storage.rs`；插件只通过接口读写。需要关系查询、唯一约束或事务的复杂数据可使用专用表和双数据库迁移，内置 `mail-template` 是现有例子；专用表不享受通用 KV 的自动加密。
5. 需要管理业务资源时实现 `manage`。核心提供 `/api/v1/admin/plugins/{id}/{resource}` 的列表、详情、创建、更新、删除路由及管理员认证和 CSRF 检查。需要专用页面时，将 Vue 组件放在 `frontend/src/plugins/<Name>.vue`，并在 `frontend/src/plugins/registry.js` 注册；`managementComponent` 是资源管理页，`configComponent` 是可选的自定义配置编辑器。未注册配置编辑器时使用通用 JSON 编辑器。

例如，配置校验可以直接复用插件的 Serde 类型：

```rust
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MyConfig {
    enabled_feature: bool,
}

fn validate_config(&self, value: &serde_json::Value) -> Result<(), PluginError> {
    serde_json::from_value::<MyConfig>(value.clone())
        .map(|_| ())
        .map_err(|error| PluginError::new(format!("invalid config: {error}")))
}
```

`PluginContext.storage` 也可在钩子或 `manage` 中使用，例如 `context.storage.put("settings/v1", &value).await?`。存储 API 错误应转换为 `PluginError`；插件不要把错误详情直接返回给发送 API 调用方。

## 代码分层与接入边界

- `crates/postwarden-plugin-api` 定义公共 `Plugin`、上下文、存储及钩子类型。它是插件与宿主之间的契约，不放置内置插件的具体业务实现。
- `src/plugin_runtime/mod.rs` 是宿主运行时，负责插件管理、注册后的调用编排、管理 API 路由及通用宿主能力；`src/plugin_runtime/storage.rs` 实现通用插件存储。
- `src/plugins/<name>.rs` 存放单个插件的具体实现。`src/plugins/mod.rs` 只声明插件模块，并在构建期创建和注册内置插件实例，不承载运行时实现或公共契约。
- 因此，维护现有插件通常只需要编辑 `src/plugins/<name>.rs`。新增一个内置插件时，添加实现文件，并在 `src/plugins/mod.rs` 声明模块和加入构建期注册；不需要修改公共 trait 或宿主运行时。只有新增所有插件都需要的宿主能力或改变插件与宿主的契约时，才修改对应的 API 或 runtime。
- 前端专用管理或配置页面放在 `frontend/src/plugins/<Name>.vue`，并在 `frontend/src/plugins/registry.js` 注册。前端注册属于独立的构建期接入步骤。

## 当前边界

前后端均为构建期注册，不支持上传插件包或热加载。通用 KV 适合少量按键读写的数据；它不提供查询、事务或自动数据迁移。插件升级时应自行兼容私有数据结构；有尚未处理的暂停消息时，核心会阻止版本升级或停用插件。
