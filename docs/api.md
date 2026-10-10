# PostWarden 主要 API 设计

本文描述 MVP 的外部契约与状态语义。路径和对象名称在正式 OpenAPI 规范生成前可做小范围命名调整，但不能改变认证、授权、同步/异步、幂等及发送结果的含义。所有接口使用 HTTPS；管理页面与 API 同源。

## 认证域

- `/api/v1/admin/**`：管理员会话 Cookie。登录、退出和修改操作使用 CSRF 防护。API Token 不可调用。
- `/api/v1/messages/**`：`Authorization: Bearer <token>`。管理员会话不自动获得发送 API 权限。
- 管理员用于 SMTP 测试的操作属于 Admin API，审计时标记管理员操作者，不伪装成业务 Token。

## 发送请求

共同字段：

| 字段 | 说明 |
| --- | --- |
| `sender_identity_id` | 必填；必须在当前 Token 的显式授权范围内 |
| `to` | 必填，至少一个收件地址 |
| `cc`, `bcc` | 可选；密送地址只进入 SMTP 信封，不写入对外邮件头 |
| `subject` | 必填；禁止换行注入邮件头 |
| `text`, `html` | 至少提供一个；同时提供时构造文本与 HTML 替代正文 |
| `attachments` | 可选二进制文件；限制单件、总量和请求总大小；不接受任意远程 URL 作为附件来源 |

无附件请求可用 `application/json`；含附件请求使用 `multipart/form-data`，其中 `message` 为上述 JSON 字段，其余为附件文件。正式 OpenAPI 会精确定义数组编码、文件字段及上限。调用方不得提供 SMTP 主机、用户名、密码、任意 `From` 或任意邮件头。

两个发送入口均要求 `Idempotency-Key` 请求头。幂等键在同一 Token 下跨入口唯一，指向同一请求内容及发送模式；复用键但改变内容或模式返回冲突。幂等记录与审计记录默认共同保留 90 天，到期后不再保证该键去重。

## 模板插件

模板发送仍使用 `POST /api/v1/messages`（异步）或 `POST /api/v1/messages/send`（同步）。请求中的 `template` 指定构建期插件及其输入；其他邮件字段可省略，由插件在邮件成形阶段填写或替换：

```json
{
  "template": {
    "plugin_id": "mail-template",
    "input": {
      "scenario_id": "order.paid",
      "variables": {"order_id": "A1001", "amount": "99.00"}
    }
  }
}
```

`mail-template` 插件可替换发件身份、主题、文本/HTML 正文和 `to`、`cc`、`bcc` 地址。模板中的 `{{order_id}}` 等占位符从 `variables` 取值；变量缺失或类型不合适时拒绝请求。HTML 正文中的变量会转义。若模板启用 `allow_caller_recipients`，请求提供的地址会追加到固定地址；否则由模板地址取代请求地址。最终邮件必须至少有一个 To 地址。调用方仍需持有模板所选发件身份的 Token 授权。

核心只按 `template.plugin_id` 找到已启用插件，并把 `template.input` 交给该插件的 `compose` 钩子。插件返回成形后的草稿，核心重新校验地址、内容和发件身份权限，再进入普通发送钩子与 SMTP 流程。插件可通过 `manage` 暴露受管理员会话和 CSRF 保护的管理资源；`mail-template` 的资源位于 `/api/v1/admin/plugins/mail-template/templates`。停用插件后，新模板请求会被拒绝，管理资源返回 `409 plugin_disabled`；重新启用后可继续管理已有模板。

模板请求的幂等比较基于插件 ID、插件输入、请求中的邮件字段及发送模式。同一个键重试时返回原消息，即使模板后来修改、删除或插件停用；更换键才会按当前配置重新成形。

## 发送与查询接口

### `POST /api/v1/messages/send`：同步发送

在当前 HTTP 请求中执行提交前与发送前插件、SMTP 发送并记录结果。SMTP 服务器确认接受时返回 `200 OK`，响应含 `message_id`、`status: smtp_accepted`、`smtp_accepted_at`。明确失败时返回相应错误码，同时提供 `message_id` 与可查询状态；SMTP 结果不确定时使用 `outcome_unknown`，调用方不可仅凭超时自行重发。

若插件要求暂停，返回 `409 Conflict`、`code: requires_async`，不发送邮件；该请求以 `rejected` 留下消息和审计记录。调用方改用异步入口时须提交新幂等键，因为发送模式不同。若相同幂等请求仍在处理，返回 `409 Conflict`、原 `message_id` 与状态查询地址，不再启动新的发送；原请求已结束则返回其既有结果。

### `POST /api/v1/messages`：异步发送

授权和请求校验通过，且加密内容与任务成功持久化后，返回 `202 Accepted`，示例：

```json
{
  "message_id": "msg_…",
  "status": "queued",
  "status_url": "/api/v1/messages/msg_…"
}
```

`202` 不承诺最终 SMTP 接受。相同幂等请求返回原消息及当前状态，不重复入队。插件可将异步任务转入 `held`；待发送期限到期后转为 `expired`。

### `GET /api/v1/messages/{message_id}`：状态查询

仅消息创建 Token 或管理员可查询；普通 Token 不可读取其他 Token 的消息。返回消息 ID、模式、Sender Identity、状态、创建与结束时间、尝试次数和经过清理的错误类别。按权限返回收件人；不返回 SMTP 凭证、正文或附件。Token 已失效时不能继续凭它查询，管理员仍可在保留期内查看审计。

状态集合见[数据模型](data-model.md)。`smtp_accepted` 仅表示上游 SMTP 接受；后续退信不在 MVP 的接收范围内。`outcome_unknown` 必须以独立状态呈现，不伪装为成功或失败。

## 管理接口（主要资源）

| 路径 | 主要操作 |
| --- | --- |
| `/api/v1/admin/session` | 登录、获取当前会话、退出 |
| `/api/v1/admin/smtp-accounts` | 创建、列表、读取、更新、删除、启停 SMTP Account |
| `/api/v1/admin/smtp-accounts/{id}/test-connection` | 测试连接与认证；不发送测试邮件 |
| `/api/v1/admin/smtp-accounts/{id}/test-message` | 向管理员指定地址发送测试邮件，并记录管理员审计事件 |
| `/api/v1/admin/sender-identities` | 管理 Sender Identity 及与 SMTP Account 的关联 |
| `/api/v1/admin/tokens` | 创建、列表、读取元数据、更新权限或有效期、禁用及撤销 Token；明文只在创建时出现 |
| `/api/v1/admin/messages` | 按时间、Token、身份、状态查询发送历史与尝试记录 |
| `/api/v1/admin/messages/{id}/resume` | 管理员请求继续处理插件暂停的异步消息；Worker 再次执行发送前校验 |
| `/api/v1/admin/messages/{id}/reject` | 管理员拒绝插件暂停的消息并进入终态 |
| `/api/v1/admin/plugins` | 查看构建中已注册插件、启停和配置；需要处理插件持有的待办状态后才能禁用相关插件 |
| `/api/v1/admin/plugins/{id}/{resource}` | 插件管理资源；`mail-template/templates` 用于创建、列出和维护邮件模板 |

SMTP 凭证的读取响应永不回传密码或密文；更新时可以不提供密码以保持原值。删除账号或身份会立即停止使用并处理关联待发送任务，保留必要的历史标识供审计引用；账号凭证随删除擦除。禁用持有待办任务的插件前必须先处理其暂停状态。

创建 Sender Identity 时，`envelope_from` 可以省略或留空；服务端会使用 `email_address` 作为退信地址。更新时省略 `envelope_from` 会保留原值，传空字符串则重置为当前 `email_address`。如需单独接收退信，可传入获 SMTP 服务商允许使用的邮箱地址。

## 错误与重试约定

错误响应包含稳定的 `code`、人类可读的 `message` 和适用时的 `message_id`。主要 HTTP 类别：`400/422` 请求格式错误、`401` 未认证、`403` 无身份权限、`409` 幂等冲突或同步流程要求异步、`413` 内容超限、`429` 请求限流、`5xx` 内部或上游故障。错误正文不得包含 SMTP 密码、API Token、完整邮件正文或未经清理的上游原始诊断。

异步 Worker 仅在确认邮件未被 SMTP 接受时对临时错误有限重试。调用方重试 HTTP 请求必须沿用原 `Idempotency-Key`；更换键代表新请求，可能造成重复发送。SMTP 发送后的不确定状态需人工判断，不自动重发。
