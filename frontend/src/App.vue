<template>
  <main class="shell">
    <header class="topbar">
      <a class="brand" href="/" aria-label="PostWarden 首页">
        <span class="brand-mark" aria-hidden="true">P</span>
        <span>PostWarden</span>
      </a>
      <span class="environment"><span class="dot"></span>本地服务</span>
    </header>

    <section class="content" aria-labelledby="page-title">
      <p class="eyebrow">管理控制台</p>
      <h1 id="page-title">发送服务正在搭建中</h1>
      <p class="intro">
        PostWarden 将在此管理 SMTP 账号、发件身份、API Token 和发送记录。
        当前页面仅用于验证前端构建与同源托管入口。
      </p>

      <div class="status-card">
        <div class="status-icon" aria-hidden="true">⌁</div>
        <div>
          <h2>管理功能尚未启用</h2>
          <p>服务骨架已就绪，后续功能会接入同一服务地址。</p>
        </div>
        <span class="status-label">准备中</span>
      </div>

      <footer>PostWarden <span>·</span> 自托管邮件发送代理</footer>
    </section>
  </main>
</template>
