# PostWarden

PostWarden 是自托管的 SMTP 发送代理和授权中心。调用方使用 PostWarden API Token 发信，SMTP 凭证由服务端管理。

项目正在从架构设计进入实现。当前仓库包含 [MVP 范围](docs/mvp-scope.md)、[架构与决策](docs/README.md)、单管理员会话认证及 SMTP 账号管理。服务启动时会连接 SQLite 或 MySQL 并执行对应迁移；发件身份、API Token、实际 SMTP 测试与发送、队列和插件尚未实现，不能用于实际发信。

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

服务默认监听 `127.0.0.1:8080`，健康检查为 `/health`（会检查数据库连接）。管理页面同源提供登录和 SMTP 账号的创建、查看、修改、启停与删除；删除账号会擦除其保存的凭证。`POSTWARDEN_LISTEN` 可指定监听地址，`POSTWARDEN_WEB_DIST` 可指定前端构建目录。`DATABASE_URL` 默认为 `sqlite://data/postwarden.db`；SQLite 数据目录会自动创建。MySQL 模式须先建立数据库并设置连接 URL。配置示例见 [`.env.example`](.env.example)，程序目前不会自动读取该文件。

## 许可证

本项目使用 [Apache License 2.0](LICENSE)。
