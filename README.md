# PostWarden

PostWarden 是自托管的 SMTP 发送代理和授权中心。调用方使用 PostWarden API Token 发信，SMTP 凭证由服务端管理。

仓库包含 [MVP 范围](docs/mvp-scope.md)、[架构与决策](docs/README.md) 和 Rust 服务。服务提供管理员会话、SMTP 账号与发件身份、API Token、发送 API、消息历史和同进程 Worker。SQLite 与 MySQL 各有迁移；MySQL 的运行时端到端验证仍待完成。

## 本地快速启动

需要 Rust 工具链、Node.js/npm 和 OpenSSL。在仓库根目录执行：

```sh
./scripts/postwarden.sh init
./scripts/postwarden.sh start
```

`init [管理员登录名]` 默认使用 `admin`，会检查依赖、执行 `npm ci`、构建管理页面和 Rust 服务、创建仅当前用户可读的 `.env`（包含一次生成的主密钥）、连接 SQLite 并执行迁移，然后交互式要求设置至少 12 字符的管理员密码。重复执行 `init` 会重新构建并检查数据库，但不会覆盖 `.env` 或已存在的管理员。完成后用 `start` 一键运行服务，按 Ctrl+C 停止。

在没有交互式终端的自动化环境中，可设置 `POSTWARDEN_SETUP_PASSWORD_FILE` 指向仅文件所有者可读的密码文件；初始化会读取它而不显示密码。初始化完成后应移除该文件。

本地默认访问 [http://127.0.0.1:8080](http://127.0.0.1:8080)，健康检查为 `/health`。`.env` 默认使用 `sqlite://data/postwarden.db`，数据目录自动创建；默认 `POSTWARDEN_COOKIE_SECURE=false` 仅适用于本地 HTTP。需要更改监听地址、数据库或保留期时，编辑 `.env` 后重新执行 `start`。`POSTWARDEN_PENDING_DAYS`、`POSTWARDEN_CONTENT_DAYS`、`POSTWARDEN_AUDIT_DAYS` 默认分别为 7、7、90 天。`.env` 与主密钥必须一起安全保存；丢失主密钥将无法解密已保存的 SMTP 凭证和邮件内容。配置字段可参见 [`.env.example`](.env.example)。

启动脚本在本地和镜像中相同：容器默认执行 `postwarden-launcher start`；初始化时执行 `postwarden-launcher init admin`。两种方式都由 Rust 服务运行迁移并启动同进程 Worker。

## 场景邮件模板

管理员可在「插件」中启用 `mail-template`，再通过该插件的「管理」入口进入模板专页：设置场景 ID、发件身份、主题及正文占位符，并配置固定收件人和抄送人。插件停用时管理入口不可用，管理 API 也会拒绝请求。调用方继续使用普通邮件发送 API，提供模板插件 ID、场景 ID、变量和幂等键。例如异步发送：

```sh
curl -X POST https://postwarden.example.com/api/v1/messages \
  -H 'Authorization: Bearer <token>' \
  -H 'Idempotency-Key: order-paid-A1001' \
  -H 'Content-Type: application/json' \
  -d '{"template":{"plugin_id":"mail-template","input":{"scenario_id":"order.paid","variables":{"order_id":"A1001"}}}}'
```

同步入口仍为 `/api/v1/messages/send`。模板插件形成草稿后，核心核对 Token 对发件身份的授权并执行原有发送流程。管理员可以在「插件」中停用 `mail-template`；停用后新的模板请求会被拒绝。其他构建期插件可实现 `compose` 和 `manage` 来扩展成形规则与管理资源。详见 [API 说明](docs/api.md)。

## SMTP 账号与测试邮件

在管理页面的「SMTP 账号」中填写发信服务器，而非 IMAP 收信服务器。端口与加密方式需要配套：465 通常选择「隐式 TLS」（连接建立时立即加密，也称 SSL/TLS），587 通常选择「STARTTLS」（连接后升级为加密）；以邮箱服务商提供的配置为准。「PLAIN」和「LOGIN」是 SMTP 账号认证方式，不代表连接是否加密。用户名通常是完整邮箱地址，密码通常是邮箱服务商生成的客户端专用密码或授权码。保存凭证后，页面不会回显密码。

飞书邮箱可先使用 `smtp.feishu.cn`、465、「隐式 TLS」、完整邮箱地址和页面显示的 IMAP/SMTP 密码；认证方式先试「LOGIN」，若服务端提示认证机制不支持，再试「PLAIN」。也可以使用 587 与「STARTTLS」。发送测试邮件时，发件地址应使用该账号获准发送的地址。

「测试连接」只验证 SMTP 连接；「发送测试邮件」显示“SMTP 服务器已接收”时，仅表示发信服务器接受了提交，**不代表目标邮箱已经投递**。若收件箱没有邮件，请确认收件地址完整且无拼写错误，搜索主题 `PostWarden SMTP test`，检查垃圾箱和企业隔离区，并查看发件邮箱的已发送邮件与退信通知。若仍未找到，使用邮箱服务商的投递日志或联系其管理员按发件人、收件人和测试时间查询。不要反复点击发送，以免产生重复邮件。

## Docker 与 HTTPS

提供 [SQLite Compose 示例](deploy/compose.sqlite.yml) 和 [MySQL Compose 示例](deploy/compose.mysql.yml)。两者使用 Caddy 提供 HTTPS；请将域名解析到部署主机，并设置持久保存的 `POSTWARDEN_MASTER_KEY`、`POSTWARDEN_DOMAIN`，MySQL 模式还要设置数据库密码。以 SQLite 为例（主密钥只生成一次并妥善保存）：

```sh
docker compose -f deploy/compose.sqlite.yml build
docker compose -f deploy/compose.sqlite.yml run --rm postwarden init admin
docker compose -f deploy/compose.sqlite.yml up -d
```

也可以将服务置于已有 TLS 网关之后，此时保持 `POSTWARDEN_COOKIE_SECURE=true`。单实例运行 Worker；不要同时启动多个 PostWarden 服务实例。需要备份数据库与主密钥，备份中可能包含尚未清理的加密邮件内容，备份保留周期应独立管理。

## 许可证

本项目使用 [Apache License 2.0](LICENSE)。
