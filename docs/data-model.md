# PostWarden 初步数据模型

本文件定义实体、关系和必须保持的约束；列类型、索引细节与迁移 SQL 在工程初始化后的设计审查中落实。SQLite 与 MySQL 必须实现相同的业务语义，各自使用适配的迁移文件。所有时间在存储与 API 中使用 UTC。

## 实体与关系

| 实体 | 主要字段 | 约束与用途 |
| --- | --- | --- |
| `admin_users` | `id`, `login_name`, `password_hash`, `created_at`, `updated_at` | MVP 仅允许一个管理员；无预置密码，由本地初始化命令创建 |
| `admin_sessions` | `id`, `admin_id`, `session_digest`, `expires_at`, `revoked_at` | 只存会话标识摘要；登录、退出与过期可撤销 |
| `smtp_accounts` | `id`, `name`, `host`, `port`, `tls_mode`, `auth_type`, `username`, `credential_ciphertext`, `enabled`, `deleted_at`, `created_at`, `updated_at` | SMTP 凭证加密保存；删除时停止使用并擦除凭证，必要的历史标识保留至审计到期 |
| `sender_identities` | `id`, `smtp_account_id`, `display_name`, `email_address`, `envelope_from`, `version`, `enabled`, `deleted_at`, `created_at`, `updated_at` | 多个身份可关联同一账号；`version` 用于检测排队期间关键属性变化 |
| `api_tokens` | `id`, `name`, `purpose`, `lookup_prefix`, `secret_digest`, `status`, `expires_at`, `created_at`, `last_used_at`, `revoked_at` | 明文仅创建时返回；`status` 区分启用、禁用、撤销；撤销不可恢复 |
| `token_sender_permissions` | `token_id`, `sender_identity_id`, `created_at` | Token 与 Sender Identity 的多对多授权；联合唯一，默认无授权 |
| `messages` | `id`, `token_id`, `sender_identity_id`, `identity_version`, `mode`, `status`, `idempotency_key`, `request_digest`, `recipient_ciphertext`, `created_at`, `queued_at`, `expires_at`, `finished_at`, `next_attempt_at`, `last_error_code` | 一次请求对应一个消息；`(token_id, idempotency_key)` 唯一，跨同步和异步入口生效 |
| `message_payloads` | `message_id`, `payload_ciphertext`, `key_version`, `retained_until` | 异步任务正文、附件及处理后内容的加密暂存；与审计记录分开清理 |
| `message_attempts` | `id`, `message_id`, `attempt_no`, `started_at`, `finished_at`, `smtp_stage`, `result`, `smtp_code`, `safe_error_code` | 每次实际 SMTP 尝试一条；仅保存经过清理的诊断信息，不保存凭证与正文 |
| `message_events` | `id`, `message_id`, `event_type`, `plugin_id`, `created_at`, `safe_details` | 记录入队、授权失败、插件拒绝或暂停、状态变更及清理等生命周期事件 |
| `plugin_configs` | `plugin_id`, `plugin_version`, `enabled`, `execution_order`, `config_ciphertext`, `updated_at` | 配置构建时已注册的插件；可能包含秘密的配置加密存储 |
| `plugin_message_states` | `message_id`, `plugin_id`, `plugin_version`, `state_ciphertext`, `resume_at` | 暂停及恢复状态；插件禁用或升级不得使等待中的任务丢失状态 |

`message_payloads` 和 `plugin_message_states` 中与邮件内容有关的数据遵守同一终态保留规则。插件若需要独立的模板等数据，使用带 `plugin_id` 命名空间的存储接口及其专用迁移或版本管理机制，不直接依赖具体数据库的 SQL。

收件人属于审计所需的敏感元数据：在消息记录中加密保留，并在有权查询时解密。MVP 的历史筛选以时间、Token、发件身份和状态为主；不以收件人全文搜索作为前提。密送人不得写入普通日志或邮件头。

## 消息状态

| 状态 | 含义 |
| --- | --- |
| `queued` | 异步请求已持久化，等待处理 |
| `processing` | 同步请求或 Worker 正在执行流程或 SMTP 尝试 |
| `retry_wait` | 已确定尚未被 SMTP 接受，等待有限次重试 |
| `held` | 插件暂停，等待审核、时间窗口或其他插件动作 |
| `smtp_accepted` | SMTP 服务器已接受邮件；不代表最终投递到收件箱 |
| `rejected` | 核心策略或插件明确拒绝，含同步流程需要异步处理；未执行 SMTP 发送 |
| `failed` | 明确失败且不再重试，包括永久 SMTP 错误或发送前权限失效 |
| `outcome_unknown` | SMTP 是否接受无法判断；不自动重发 |
| `expired` | 异步任务超过待发送期限，未开始新的发送尝试 |

`smtp_accepted`、`rejected`、`failed`、`outcome_unknown`、`expired` 均为终态。到期检查在开始新的 SMTP 尝试前进行；已经开始的 SMTP 会话不能因到期而被误判为“未发送”。同步请求也生成消息和审计记录，但不持久化正文、附件。

## 幂等与权限不变量

1. Token 仅能使用 `token_sender_permissions` 明确列出的 Sender Identity；不能通过 SMTP Account 隐式取得其全部身份权限。
2. 创建消息与占用幂等键在同一事务中完成。相同 Token、键、请求摘要和发送模式只对应同一消息；摘要或模式不同返回冲突，绝不创建第二条待发送任务。
3. 异步发送前再次确认 Token 未禁用、未撤销、未过期，身份和 SMTP Account 均启用，身份关键属性版本未改变。失败则记录终态，不继续 SMTP。
4. Worker 对任务的领取与状态变更必须原子化。进程在 `processing` 中中断且无法证明 SMTP 未接受时，恢复为 `outcome_unknown`，不得简单回到 `queued`。
5. 插件修改后的最终内容在发送前持久化；重试使用该版本，不重复执行非幂等的修改钩子。

## 保留与清理

三个期限分别配置，默认值已确认：

| 配置语义 | 默认值 | 到期行为 |
| --- | --- | --- |
| 待发送最长时间 | 7 天 | `queued`、`retry_wait` 或 `held` 任务转为 `expired`，停止后续发送 |
| 终态内容保留时间 | 7 天 | 从进入终态开始计时；到期后清理加密正文、附件及关联的内容型插件状态，`expired` 与其他终态完全相同 |
| 审计与幂等记录保留时间 | 90 天 | 到期后清理消息元数据、尝试与事件；幂等保证也随记录到期结束 |

清理过程必须可重复执行，不能因进程重启而漏删。终态内容保留时间即使设为零，也是在任务进入终态后执行清理，不会让 `expired` 在状态落库前丢失内容。每个任务入队时固定待发送截止时间，进入终态时固定内容清理时间；之后修改配置只影响新计算的期限。审计期限从任务进入终态开始计算。备份也可能包含尚未到期或此前已删除的密文；备份保留和密钥管理需要在部署文档中说明。
