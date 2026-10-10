# PostWarden

PostWarden 是自托管的 SMTP 发送代理和授权中心。调用方使用 PostWarden API Token 发信，SMTP 凭证由服务端管理。

项目正在从架构设计进入实现。当前仓库包含 [MVP 范围](docs/mvp-scope.md)、[架构与决策](docs/README.md)及最小工程骨架；账号管理、认证、发送、队列、插件和数据库持久化尚未实现，不能用于实际发信。

## 本地开发

需要 Rust 工具链、Node.js 和 npm。首次启动前构建管理页面：

```sh
cd frontend
npm ci
npm run build
cd ..
cargo run
```

服务默认监听 `127.0.0.1:8080`，健康检查为 `/health`。`POSTWARDEN_LISTEN` 可指定监听地址，`POSTWARDEN_WEB_DIST` 可指定前端构建目录。`DATABASE_URL` 默认为 `sqlite://data/postwarden.db`；当前仅校验数据库类型，尚未连接数据库。配置示例见 [`.env.example`](.env.example)，程序目前不会自动读取该文件。

## 许可证

本项目使用 [Apache License 2.0](LICENSE)。
