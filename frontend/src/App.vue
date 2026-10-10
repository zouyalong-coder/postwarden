<script setup>
import { onMounted, reactive, ref } from 'vue'

const session = ref({ authenticated: false, login_name: null })
const login = reactive({ login_name: '', password: '' })
const accounts = ref([])
const loading = ref(true)
const busy = ref(false)
const notice = ref('')
const error = ref('')
const editingId = ref(null)

const emptyForm = () => ({ name: '', host: '', port: 587, tls_mode: 'starttls', auth_type: 'plain', username: '', credential: '', enabled: true })
const form = reactive(emptyForm())

function csrfToken() {
  const cookie = document.cookie.split('; ').find((item) => item.startsWith('postwarden_csrf='))
  return cookie ? decodeURIComponent(cookie.slice('postwarden_csrf='.length)) : ''
}

async function api(path, { method = 'GET', body } = {}) {
  const headers = {}
  if (body) headers['Content-Type'] = 'application/json'
  if (method !== 'GET') headers['X-CSRF-Token'] = csrfToken()
  const response = await fetch(`/api/v1/admin${path}`, {
    method, headers, credentials: 'same-origin', cache: 'no-store',
    body: body ? JSON.stringify(body) : undefined,
  })
  if (!response.ok) {
    const details = await response.json().catch(() => ({}))
    throw new Error(details.message || `请求失败 (${response.status})`)
  }
  return response.status === 204 ? null : response.json()
}

function clearNotice() { notice.value = ''; error.value = '' }
async function loadAccounts() { accounts.value = await api('/smtp-accounts') }

async function loadSession() {
  try {
    session.value = await api('/session')
    if (session.value.authenticated) await loadAccounts()
  } catch (cause) { error.value = cause.message }
  finally { loading.value = false }
}

async function signIn() {
  clearNotice(); busy.value = true
  try {
    session.value = await api('/session', { method: 'POST', body: { ...login } })
    login.password = ''
    await loadAccounts()
  } catch (cause) { error.value = cause.message }
  finally { busy.value = false }
}

async function signOut() {
  clearNotice(); busy.value = true
  try {
    await api('/session', { method: 'DELETE' })
    session.value = await api('/session')
    accounts.value = []
    cancelEdit()
  } catch (cause) { error.value = cause.message }
  finally { busy.value = false }
}

function edit(account) {
  clearNotice()
  editingId.value = account.id
  Object.assign(form, { name: account.name, host: account.host, port: account.port,
    tls_mode: account.tls_mode, auth_type: account.auth_type, username: account.username || '',
    credential: '', enabled: account.enabled })
  document.getElementById('account-form')?.scrollIntoView({ behavior: 'smooth' })
}

function cancelEdit() { editingId.value = null; Object.assign(form, emptyForm()) }

async function saveAccount() {
  clearNotice(); busy.value = true
  const body = { name: form.name.trim(), host: form.host.trim(), port: Number(form.port),
    tls_mode: form.tls_mode, auth_type: form.auth_type,
    username: form.auth_type === 'none' ? null : form.username.trim(), enabled: form.enabled }
  if (form.credential) body.credential = form.credential
  try {
    if (editingId.value) {
      await api(`/smtp-accounts/${editingId.value}`, { method: 'PUT', body })
      notice.value = '账号已更新。'
    } else {
      await api('/smtp-accounts', { method: 'POST', body })
      notice.value = '账号已创建。'
    }
    cancelEdit()
    await loadAccounts()
  } catch (cause) { error.value = cause.message }
  finally { busy.value = false }
}

async function toggleAccount(account) {
  clearNotice(); busy.value = true
  try {
    await api(`/smtp-accounts/${account.id}/${account.enabled ? 'disable' : 'enable'}`, { method: 'POST' })
    await loadAccounts()
  } catch (cause) { error.value = cause.message }
  finally { busy.value = false }
}

async function deleteAccount(account) {
  if (!window.confirm(`删除 SMTP 账号“${account.name}”？保存的凭证会被擦除。`)) return
  clearNotice(); busy.value = true
  try {
    await api(`/smtp-accounts/${account.id}`, { method: 'DELETE' })
    if (editingId.value === account.id) cancelEdit()
    notice.value = '账号已删除。'
    await loadAccounts()
  } catch (cause) { error.value = cause.message }
  finally { busy.value = false }
}

onMounted(loadSession)
</script>

<template>
  <main class="shell">
    <header class="topbar">
      <a class="brand" href="/"><span class="brand-mark">P</span>PostWarden</a>
      <div class="header-actions">
        <span class="environment"><span class="dot"></span>本地服务</span>
        <span v-if="session.authenticated" class="login-name">{{ session.login_name }}</span>
        <button v-if="session.authenticated" class="text-button" :disabled="busy" @click="signOut">退出</button>
      </div>
    </header>
    <section class="content">
      <p class="eyebrow">管理控制台</p>
      <h1>SMTP 发送配置</h1>
      <p class="intro">管理发送通道及其凭证。发件身份、API Token 和消息发送功能会在后续接入。</p>
      <div v-if="error" class="notice error" role="alert">{{ error }}</div>
      <div v-if="notice" class="notice success" role="status">{{ notice }}</div>
      <div v-if="loading" class="panel">正在连接服务…</div>

      <form v-else-if="!session.authenticated" class="panel login-panel" @submit.prevent="signIn">
        <div class="panel-heading"><h2>管理员登录</h2><p>首次使用前，在服务器上运行 <code>postwarden init-admin &lt;login-name&gt;</code>。</p></div>
        <label>登录名<input v-model.trim="login.login_name" autocomplete="username" required /></label>
        <label>密码<input v-model="login.password" type="password" autocomplete="current-password" required /></label>
        <button class="primary-button" :disabled="busy" type="submit">{{ busy ? '登录中…' : '登录' }}</button>
      </form>

      <template v-else>
        <section class="panel" aria-labelledby="accounts-title">
          <div class="panel-heading split-heading"><div><h2 id="accounts-title">SMTP 账号</h2><p>页面不会回显已保存的密码。</p></div><span class="count">{{ accounts.length }} 个账号</span></div>
          <p v-if="accounts.length === 0" class="empty-state">还没有 SMTP 账号。使用下方表单添加第一个发送通道。</p>
          <div v-for="account in accounts" :key="account.id" class="account-row">
            <div><div class="account-name">{{ account.name }} <span :class="['badge', account.enabled ? 'enabled' : 'disabled']">{{ account.enabled ? '已启用' : '已停用' }}</span></div><div class="account-details">{{ account.host }}:{{ account.port }} · {{ account.tls_mode }} · {{ account.username || '无需认证' }}</div></div>
            <div class="row-actions"><button class="text-button" :disabled="busy" @click="edit(account)">编辑</button><button class="text-button" :disabled="busy" @click="toggleAccount(account)">{{ account.enabled ? '停用' : '启用' }}</button><button class="text-button danger" :disabled="busy" @click="deleteAccount(account)">删除</button></div>
          </div>
        </section>

        <form id="account-form" class="panel" @submit.prevent="saveAccount">
          <div class="panel-heading split-heading"><div><h2>{{ editingId ? '编辑 SMTP 账号' : '添加 SMTP 账号' }}</h2><p>{{ editingId ? '留空密码会保留当前凭证。' : '填写服务器和认证信息。' }}</p></div><button v-if="editingId" class="text-button" type="button" @click="cancelEdit">取消编辑</button></div>
          <div class="form-grid">
            <label>账号名称<input v-model="form.name" maxlength="191" required placeholder="例如：事务邮件" /></label>
            <label>SMTP 主机<input v-model="form.host" maxlength="255" required placeholder="smtp.example.com" /></label>
            <label>端口<input v-model.number="form.port" type="number" min="1" max="65535" required /></label>
            <label>加密方式<select v-model="form.tls_mode"><option value="starttls">STARTTLS</option><option value="implicit">隐式 TLS</option><option value="none">无 TLS（仅免认证）</option></select></label>
            <label>认证方式<select v-model="form.auth_type"><option value="plain">PLAIN</option><option value="login">LOGIN</option><option value="none">无需认证</option></select></label>
            <label v-if="form.auth_type !== 'none'">用户名<input v-model="form.username" autocomplete="off" :required="!editingId" /></label>
            <label v-if="form.auth_type !== 'none'" class="wide">密码<input v-model="form.credential" type="password" autocomplete="new-password" :required="!editingId" /></label>
          </div>
          <label class="checkbox"><input v-model="form.enabled" type="checkbox" /> 启用此账号</label>
          <div class="form-actions"><button class="primary-button" :disabled="busy" type="submit">{{ busy ? '保存中…' : editingId ? '保存更改' : '创建账号' }}</button></div>
        </form>
      </template>
      <footer>PostWarden <span>·</span> 自托管邮件发送代理</footer>
    </section>
  </main>
</template>
