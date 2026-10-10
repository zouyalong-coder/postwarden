# PostWarden

PostWarden 是自托管的 SMTP 发送代理和授权中心。调用方使用 PostWarden API Token 发信，SMTP 凭证由服务端管理。

仓库包含 [MVP 范围](docs/mvp-scope.md)、[架构与决策](docs/README.md) 和 Rust 服务。服务提供管理员会话、SMTP 账号与发件身份、API Token、发送 API、消息历史和同进程 Worker。SQLite 与 MySQL 各有迁移；MySQL 的运行时端到端验证仍待完成。

## 本地开发

需要 Rust 工具链、Node.js 和 npm。首次启动前构建管理页面并初始化管理员：

```sh
cd frontend
npm ci
npm run build
cd ..
cargo run -- init-admin admin
```

初始化命令会交互式要求输入并确认至少 12 个字符的密码。仅能创建一个管理员。服务启动还需要持久保存的 32 字节主密钥；可用 `openssl rand -base64 32` 生成一次，并将结果作为 `POSTWARDEN_MASTER_KEY` 环境变量安全提供。丢失该密钥将无法解密已保存的 SMTP 凭证。本地 HTTP 开发时将 `POSTWARDEN_COOKIE_SECURE=false`，正式 HTTPS 部署保持默认的 `true`。

```sh
export POSTWARDEN_MASTER_KEY='已安全保存的 base64 密钥'
export POSTWARDEN_COOKIE_SECURE=false
cargo run
```

服务默认监听 `127.0.0.1:8080`，健康检查为 `/health`（会检查数据库连接）。`POSTWARDEN_LISTEN` 可指定监听地址，`POSTWARDEN_WEB_DIST` 可指定前端构建目录。`DATABASE_URL` 默认为 `sqlite://data/postwarden.db`；SQLite 数据目录会自动创建。MySQL 模式须先建立数据库并设置连接 URL。`POSTWARDEN_PENDING_DAYS`、`POSTWARDEN_CONTENT_DAYS` 和 `POSTWARDEN_AUDIT_DAYS` 控制保留期限，默认分别为 7、7、90 天。配置示例见 [`.env.example`](.env.example)，程序不会自动读取该文件。

## Docker 与 HTTPS

提供 [SQLite Compose 示例](deploy/compose.sqlite.yml) 和 [MySQL Compose 示例](deploy/compose.mysql.yml)。两者使用 Caddy 提供 HTTPS；请将域名解析到部署主机，并设置持久保存的 `POSTWARDEN_MASTER_KEY`、`POSTWARDEN_DOMAIN`，MySQL 模式还要设置数据库密码。以 SQLite 为例：

```sh
docker compose -f deploy/compose.sqlite.yml build
docker compose -f deploy/compose.sqlite.yml run --rm postwarden postwarden init-admin admin
docker compose -f deploy/compose.sqlite.yml up -d
```

也可以将服务置于已有 TLS 网关之后，此时保持 `POSTWARDEN_COOKIE_SECURE=true`。单实例运行 Worker；不要同时启动多个 PostWarden 服务实例。需要备份数据库与主密钥，备份中可能包含尚未清理的加密邮件内容，备份保留周期应独立管理。

## 许可证

本项目使用 [Apache License 2.0](LICENSE)。
