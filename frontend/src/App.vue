<script setup>
import { onMounted, reactive, ref } from 'vue'

const session = ref({ authenticated: false, login_name: null })
const login = reactive({ login_name: '', password: '' })
const accounts = ref([])
const identities = ref([])
const tokens = ref([])
const plugins = ref([])
const messages = ref([])
const messageDetail = ref(null)
const historyFilters = reactive({ token_id: '', sender_identity_id: '', status: '' })
const testForms = reactive({})
const testingAccountId = ref(null)
const loading = ref(true)
const busy = ref(false)
const notice = ref('')
const error = ref('')
const editingId = ref(null)
const identityEditingId = ref(null)
const tokenEditingId = ref(null)
const issuedTokenSecret = ref('')
const pluginEditingId = ref(null)
const pluginConfigText = ref('')

const emptyForm = () => ({ name: '', host: '', port: 587, tls_mode: 'starttls', auth_type: 'plain', username: '', credential: '', enabled: true })
const form = reactive(emptyForm())
const emptyIdentityForm = () => ({ smtp_account_id: '', display_name: '', email_address: '', envelope_from: '', enabled: true })
const identityForm = reactive(emptyIdentityForm())
const emptyTokenForm = () => ({ name: '', purpose: '', expires_at: '', sender_identity_ids: [] })
const tokenForm = reactive(emptyTokenForm())

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
async function loadAccounts() {
  accounts.value = await api('/smtp-accounts')
  for (const account of accounts.value) ensureTestForm(account)
}
async function loadIdentities() {
  identities.value = await api('/sender-identities')
  for (const account of accounts.value) ensureTestForm(account)
}
async function loadTokens() { tokens.value = await api('/tokens') }
async function loadPlugins() { plugins.value = await api('/plugins') }
async function loadMessages() {
  const params = new URLSearchParams({ limit: '100' })
  for (const key of ['token_id', 'sender_identity_id', 'status']) if (historyFilters[key]) params.set(key, historyFilters[key])
  messages.value = await api(`/messages?${params.toString()}`)
}
async function loadAdminData() { await Promise.all([loadAccounts(), loadIdentities(), loadTokens(), loadPlugins(), loadMessages()]) }

async function loadSession() {
  try {
    session.value = await api('/session')
    if (session.value.authenticated) await loadAdminData()
  } catch (cause) { error.value = cause.message }
  finally { loading.value = false }
}

async function signIn() {
  clearNotice(); busy.value = true
  try {
    session.value = await api('/session', { method: 'POST', body: { ...login } })
    login.password = ''
    await loadAdminData()
  } catch (cause) { error.value = cause.message }
  finally { busy.value = false }
}

async function signOut() {
  clearNotice(); busy.value = true
  try {
    await api('/session', { method: 'DELETE' })
    session.value = await api('/session')
    accounts.value = []
    identities.value = []
    tokens.value = []
    plugins.value = []
    messages.value = []
    messageDetail.value = null
    issuedTokenSecret.value = ''
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

function ensureTestForm(account) {
  if (!testForms[account.id]) {
    const identity = identities.value.find(item => item.smtp_account_id === account.id && item.enabled && !item.deleted_at)
    testForms[account.id] = { from: identity?.email_address || '', to: '', result: null }
  } else if (!testForms[account.id].from) {
    const identity = identities.value.find(item => item.smtp_account_id === account.id && item.enabled && !item.deleted_at)
    if (identity) testForms[account.id].from = identity.email_address
  }
}
function testForm(account) { return testForms[account.id] }
async function testConnection(account) {
  clearNotice(); busy.value = true; testingAccountId.value = account.id
  const result = testForms[account.id]
  result.result = null
  try {
    result.result = await api(`/smtp-accounts/${account.id}/test-connection`, { method: 'POST' })
  } catch (cause) { error.value = cause.message }
  finally { busy.value = false; testingAccountId.value = null }
}
async function sendTestMessage(account) {
  clearNotice(); busy.value = true; testingAccountId.value = account.id
  const result = testForms[account.id]
  result.result = null
  try {
    result.result = await api(`/smtp-accounts/${account.id}/test-message`, { method: 'POST',
      body: { from: result.from.trim(), to: result.to.trim() } })
  } catch (cause) { error.value = cause.message }
  finally { busy.value = false; testingAccountId.value = null }
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

function editIdentity(identity) {
  clearNotice()
  identityEditingId.value = identity.id
  Object.assign(identityForm, { smtp_account_id: identity.smtp_account_id, display_name: identity.display_name,
    email_address: identity.email_address, envelope_from: identity.envelope_from, enabled: identity.enabled })
  document.getElementById('identity-form')?.scrollIntoView({ behavior: 'smooth' })
}
function cancelIdentityEdit() { identityEditingId.value = null; Object.assign(identityForm, emptyIdentityForm()) }
async function saveIdentity() {
  clearNotice(); busy.value = true
  const body = { ...identityForm, display_name: identityForm.display_name.trim(),
    email_address: identityForm.email_address.trim(), envelope_from: identityForm.envelope_from.trim() }
  try {
    if (identityEditingId.value) {
      await api(`/sender-identities/${identityEditingId.value}`, { method: 'PUT', body })
      notice.value = '发件身份已更新。'
    } else {
      await api('/sender-identities', { method: 'POST', body })
      notice.value = '发件身份已创建。'
    }
    cancelIdentityEdit()
    await loadIdentities()
  } catch (cause) { error.value = cause.message }
  finally { busy.value = false }
}
async function toggleIdentity(identity) {
  clearNotice(); busy.value = true
  try {
    await api(`/sender-identities/${identity.id}/${identity.enabled ? 'disable' : 'enable'}`, { method: 'POST' })
    await loadIdentities()
  } catch (cause) { error.value = cause.message }
  finally { busy.value = false }
}
async function deleteIdentity(identity) {
  if (!window.confirm(`删除发件身份“${identity.display_name} <${identity.email_address}>”？`)) return
  clearNotice(); busy.value = true
  try {
    await api(`/sender-identities/${identity.id}`, { method: 'DELETE' })
    if (identityEditingId.value === identity.id) cancelIdentityEdit()
    notice.value = '发件身份已删除。'
    await Promise.all([loadIdentities(), loadTokens()])
  } catch (cause) { error.value = cause.message }
  finally { busy.value = false }
}

function toIso(value) { return value ? new Date(value).toISOString() : null }
function toLocal(value) {
  if (!value) return ''
  const date = new Date(value)
  return new Date(date.getTime() - date.getTimezoneOffset() * 60000).toISOString().slice(0, 16)
}
function editToken(token) {
  clearNotice()
  tokenEditingId.value = token.id
  const activeIds = new Set(identities.value.filter(identity => identity.enabled && !identity.deleted_at
    && accounts.value.some(account => account.id === identity.smtp_account_id && account.enabled)).map(identity => identity.id))
  Object.assign(tokenForm, { name: token.name, purpose: token.purpose || '', expires_at: toLocal(token.expires_at),
    sender_identity_ids: token.sender_identity_ids.filter(id => activeIds.has(id)) })
  document.getElementById('token-form')?.scrollIntoView({ behavior: 'smooth' })
}
function cancelTokenEdit() { tokenEditingId.value = null; Object.assign(tokenForm, emptyTokenForm()) }
async function saveToken() {
  if (!tokenEditingId.value && issuedTokenSecret.value) return
  clearNotice(); busy.value = true
  const body = { name: tokenForm.name.trim(), purpose: tokenForm.purpose.trim() || null,
    expires_at: toIso(tokenForm.expires_at), sender_identity_ids: tokenForm.sender_identity_ids }
  try {
    if (tokenEditingId.value) {
      await api(`/tokens/${tokenEditingId.value}`, { method: 'PUT', body })
      notice.value = 'Token 设置已更新。'
    } else {
      const created = await api('/tokens', { method: 'POST', body })
      issuedTokenSecret.value = created.token_secret
      notice.value = 'Token 已创建。请立即复制密钥。'
    }
    cancelTokenEdit()
    await loadTokens()
  } catch (cause) { error.value = cause.message }
  finally { busy.value = false }
}
async function setTokenStatus(token, action) {
  if (action === 'revoke' && !window.confirm(`永久撤销 Token“${token.name}”？此操作无法撤回。`)) return
  clearNotice(); busy.value = true
  try {
    await api(`/tokens/${token.id}/${action}`, { method: 'POST' })
    notice.value = action === 'revoke' ? 'Token 已撤销。' : `Token 已${action === 'enable' ? '启用' : '停用'}。`
    await loadTokens()
  } catch (cause) { error.value = cause.message }
  finally { busy.value = false }
}
async function copyTokenSecret() {
  try { await navigator.clipboard.writeText(issuedTokenSecret.value); notice.value = 'Token 密钥已复制。' }
  catch { error.value = '无法访问剪贴板，请手动复制密钥。' }
}

function editPlugin(plugin) {
  clearNotice(); pluginEditingId.value = plugin.id
  pluginConfigText.value = plugin.has_config ? '' : (plugin.id === 'example-policy' ? '{\n  "subject_prefix": "[PostWarden] ",\n  "reject_subject_contains": "blocked",\n  "hold_subject_contains": "review"\n}' : '{}')
}
function cancelPluginEdit() { pluginEditingId.value = null; pluginConfigText.value = '' }
async function savePlugin(plugin) {
  clearNotice(); busy.value = true
  try {
    const config = JSON.parse(pluginConfigText.value)
    if (!config || Array.isArray(config) || typeof config !== 'object') throw new Error('配置必须是 JSON 对象。')
    await api(`/plugins/${plugin.id}`, { method: 'PUT', body: { enabled: plugin.enabled,
      execution_order: Number(plugin.execution_order), config } })
    notice.value = '插件设置已保存。'
    cancelPluginEdit()
    await loadPlugins()
  } catch (cause) { error.value = cause instanceof SyntaxError ? '插件配置不是有效的 JSON。' : cause.message }
  finally { busy.value = false }
}
async function togglePlugin(plugin) {
  clearNotice(); busy.value = true
  try {
    await api(`/plugins/${plugin.id}`, { method: 'PUT', body: { enabled: !plugin.enabled,
      execution_order: plugin.execution_order } })
    await loadPlugins()
  } catch (cause) { error.value = cause.message }
  finally { busy.value = false }
}
async function openMessage(message) {
  clearNotice(); messageDetail.value = null; busy.value = true
  try { messageDetail.value = await api(`/messages/${message.id}`) }
  catch (cause) { error.value = cause.message }
  finally { busy.value = false }
}
async function applyHistoryFilters() {
  clearNotice(); messageDetail.value = null; busy.value = true
  try { await loadMessages() }
  catch (cause) { error.value = cause.message }
  finally { busy.value = false }
}
function formatTime(value) { return value ? new Date(value).toLocaleString() : '—' }

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
      <h1>邮件发送管理</h1>
      <p class="intro">管理 SMTP 通道、发件身份、访问 Token 和构建时插件。凭证和 Token 密钥不会保存在浏览器中。</p>
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
            <div class="account-controls">
              <div class="row-actions"><button class="text-button" :disabled="busy" @click="testConnection(account)">{{ testingAccountId === account.id ? '测试中…' : '测试连接' }}</button><button class="text-button" :disabled="busy" @click="edit(account)">编辑</button><button class="text-button" :disabled="busy" @click="toggleAccount(account)">{{ account.enabled ? '停用' : '启用' }}</button><button class="text-button danger" :disabled="busy" @click="deleteAccount(account)">删除</button></div>
              <div v-if="testForm(account).result" class="test-result" :class="testForm(account).result.success ? 'test-success' : 'test-failure'">{{ testForm(account).result.message }}</div>
              <details class="test-message"><summary>发送测试邮件</summary><div class="test-message-form"><label>发件地址<input v-model="testForm(account).from" type="email" required placeholder="mailer@example.com" /></label><label>收件地址<input v-model="testForm(account).to" type="email" required placeholder="you@example.com" /></label><button class="primary-button" type="button" :disabled="busy || !testForm(account).from || !testForm(account).to" @click="sendTestMessage(account)">{{ testingAccountId === account.id ? '发送中…' : '发送固定测试邮件' }}</button><p class="helper-text">将发送固定内容的测试邮件，收件人会收到实际邮件。</p></div></details>
            </div>
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

        <section class="panel" aria-labelledby="identities-title">
          <div class="panel-heading split-heading"><div><h2 id="identities-title">发件身份</h2><p>Token 只能使用已启用的身份；身份变更会更新版本。</p></div><span class="count">{{ identities.filter(item => !item.deleted_at).length }} 个有效身份</span></div>
          <p v-if="identities.length === 0" class="empty-state">还没有发件身份。添加身份后才能创建 API Token。</p>
          <div v-for="identity in identities" :key="identity.id" class="account-row" :class="{ 'muted-row': identity.deleted_at }">
            <div><div class="account-name">{{ identity.display_name }} &lt;{{ identity.email_address }}&gt; <span :class="['badge', identity.deleted_at ? 'disabled' : identity.enabled ? 'enabled' : 'disabled']">{{ identity.deleted_at ? '已删除' : identity.enabled ? '已启用' : '已停用' }}</span></div><div class="account-details">Envelope From: {{ identity.envelope_from }} · SMTP: {{ accounts.find(account => account.id === identity.smtp_account_id)?.name || identity.smtp_account_id }} · 版本 {{ identity.version }}</div></div>
            <div v-if="!identity.deleted_at" class="row-actions"><button class="text-button" :disabled="busy" @click="editIdentity(identity)">编辑</button><button class="text-button" :disabled="busy" @click="toggleIdentity(identity)">{{ identity.enabled ? '停用' : '启用' }}</button><button class="text-button danger" :disabled="busy" @click="deleteIdentity(identity)">删除</button></div>
          </div>
        </section>

        <form id="identity-form" class="panel" @submit.prevent="saveIdentity">
          <div class="panel-heading split-heading"><div><h2>{{ identityEditingId ? '编辑发件身份' : '添加发件身份' }}</h2><p>邮箱地址和信封发件地址只接受单个邮箱格式。</p></div><button v-if="identityEditingId" class="text-button" type="button" @click="cancelIdentityEdit">取消编辑</button></div>
          <div class="form-grid">
            <label>SMTP 账号<select v-model="identityForm.smtp_account_id" required><option value="" disabled>选择已启用账号</option><option v-for="account in accounts.filter(item => item.enabled)" :key="account.id" :value="account.id">{{ account.name }} · {{ account.host }}</option><option v-if="identityEditingId && accounts.some(item => item.id === identityForm.smtp_account_id && !item.enabled)" :value="identityForm.smtp_account_id" disabled>当前账号已停用</option></select></label>
            <label>显示名称<input v-model="identityForm.display_name" maxlength="191" required placeholder="例如：PostWarden 通知" /></label>
            <label>发件邮箱<input v-model="identityForm.email_address" type="email" maxlength="320" required placeholder="mailer@example.com" /></label>
            <label>Envelope From<input v-model="identityForm.envelope_from" type="email" maxlength="320" required placeholder="bounces@example.com" /></label>
          </div>
          <label class="checkbox"><input v-model="identityForm.enabled" type="checkbox" /> 启用此身份</label>
          <div class="form-actions"><button class="primary-button" :disabled="busy || !accounts.some(item => item.enabled)" type="submit">{{ busy ? '保存中…' : identityEditingId ? '保存身份' : '创建身份' }}</button></div>
        </form>

        <section v-if="issuedTokenSecret" class="panel secret-panel" aria-live="polite">
          <div class="panel-heading"><h2>新 Token 密钥</h2><p>此密钥只显示这一次。请现在复制并妥善保存，离开此页面后无法再次查看。</p></div>
          <div class="secret-value"><code>{{ issuedTokenSecret }}</code><div class="secret-actions"><button class="primary-button" type="button" @click="copyTokenSecret">复制密钥</button><button class="text-button" type="button" @click="issuedTokenSecret = ''">我已保存</button></div></div>
        </section>

        <section class="panel" aria-labelledby="tokens-title">
          <div class="panel-heading split-heading"><div><h2 id="tokens-title">API Token</h2><p>密钥只在创建时显示；列表不会返回密钥。</p></div><span class="count">{{ tokens.length }} 个 Token</span></div>
          <p v-if="tokens.length === 0" class="empty-state">还没有 API Token。</p>
          <div v-for="token in tokens" :key="token.id" class="account-row">
            <div class="token-summary"><div class="account-name">{{ token.name }} <span :class="['badge', token.status === 'enabled' ? 'enabled' : 'disabled']">{{ token.status === 'enabled' ? '已启用' : token.status === 'revoked' ? '已撤销' : '已停用' }}</span></div><div class="account-details">前缀 {{ token.lookup_prefix }} · {{ token.purpose || '无用途说明' }} · {{ token.expires_at ? `到期 ${new Date(token.expires_at).toLocaleString()}` : '永不过期' }}</div><div class="permission-list">允许身份：{{ token.sender_identity_ids.map(id => { const item = identities.find(identity => identity.id === id); return item ? `${item.display_name} <${item.email_address}>` : id }).join('、') || '无' }}</div></div>
            <div class="row-actions"><button v-if="token.status !== 'revoked'" class="text-button" :disabled="busy" @click="editToken(token)">编辑</button><button v-if="token.status !== 'revoked'" class="text-button" :disabled="busy" @click="setTokenStatus(token, token.status === 'enabled' ? 'disable' : 'enable')">{{ token.status === 'enabled' ? '停用' : '启用' }}</button><button v-if="token.status !== 'revoked'" class="text-button danger" :disabled="busy" @click="setTokenStatus(token, 'revoke')">撤销</button></div>
          </div>
        </section>

        <form id="token-form" class="panel" @submit.prevent="saveToken">
          <div class="panel-heading split-heading"><div><h2>{{ tokenEditingId ? '编辑 API Token' : '创建 API Token' }}</h2><p>选择至少一个启用的发件身份，并可设置过期时间。</p></div><button v-if="tokenEditingId" class="text-button" type="button" @click="cancelTokenEdit">取消编辑</button></div>
          <div class="form-grid">
            <label>名称<input v-model="tokenForm.name" maxlength="191" required placeholder="例如：订单服务" /></label>
            <label>用途说明<input v-model="tokenForm.purpose" maxlength="255" placeholder="例如：发送订单通知" /></label>
            <label>过期时间<input v-model="tokenForm.expires_at" type="datetime-local" /></label>
            <label>允许的发件身份<select v-model="tokenForm.sender_identity_ids" multiple required size="4" class="identity-select"><option v-for="identity in identities.filter(item => item.enabled && !item.deleted_at && accounts.some(account => account.id === item.smtp_account_id && account.enabled))" :key="identity.id" :value="identity.id">{{ identity.display_name }} · {{ identity.email_address }}</option></select><small>按住 Ctrl（macOS 上按住 ⌘）可选择多个。</small></label>
          </div>
          <div class="form-actions"><button class="primary-button" :disabled="busy || tokenForm.sender_identity_ids.length === 0 || (!tokenEditingId && !!issuedTokenSecret)" type="submit">{{ busy ? '保存中…' : tokenEditingId ? '保存 Token 设置' : issuedTokenSecret ? '先保存上一个密钥' : '创建 Token' }}</button></div>
        </form>

        <section class="panel" aria-labelledby="plugins-title">
          <div class="panel-heading split-heading"><div><h2 id="plugins-title">构建时插件</h2><p>插件配置在服务端加密保存。这里不会回显配置内容。</p></div><span class="count">{{ plugins.length }} 个插件</span></div>
          <p v-if="plugins.length === 0" class="empty-state">当前构建没有注册插件。</p>
          <div v-for="plugin in plugins" :key="plugin.id" class="account-row plugin-row">
            <div class="token-summary"><div class="account-name">{{ plugin.id }} <span :class="['badge', plugin.enabled ? 'enabled' : 'disabled']">{{ plugin.enabled ? '已启用' : '已停用' }}</span></div><div class="account-details">版本 {{ plugin.version }} · 执行顺序 {{ plugin.execution_order }} · {{ plugin.has_config ? '已有加密配置' : '使用默认配置' }}</div>
              <div v-if="pluginEditingId === plugin.id" class="plugin-editor"><label>完整 JSON 配置<textarea v-model="pluginConfigText" rows="6" spellcheck="false" :placeholder="plugin.has_config ? '请输入完整的新配置 JSON 对象' : '{}'" required></textarea><small>出于安全原因，已有配置不会回显。保存时会用此 JSON 完整替换原配置。</small></label><div class="row-actions"><button class="primary-button" :disabled="busy || !pluginConfigText.trim()" @click="savePlugin(plugin)">保存设置</button><button class="text-button" :disabled="busy" @click="cancelPluginEdit">取消</button></div></div>
            </div>
            <div v-if="pluginEditingId !== plugin.id" class="row-actions"><button class="text-button" :disabled="busy" @click="editPlugin(plugin)">配置</button><button class="text-button" :disabled="busy" @click="togglePlugin(plugin)">{{ plugin.enabled ? '停用' : '启用' }}</button></div>
          </div>
        </section>

        <section class="panel" aria-labelledby="history-title">
          <div class="panel-heading split-heading"><div><h2 id="history-title">发送历史</h2><p>仅显示状态和审计信息，不包含邮件正文、收件人或附件。</p></div><button class="text-button" :disabled="busy" @click="applyHistoryFilters">刷新记录</button></div>
          <div class="form-grid history-filters">
            <label>Token<select v-model="historyFilters.token_id"><option value="">全部 Token</option><option v-for="token in tokens" :key="token.id" :value="token.id">{{ token.name }} · {{ token.lookup_prefix }}</option></select></label>
            <label>发件身份<select v-model="historyFilters.sender_identity_id"><option value="">全部身份</option><option v-for="identity in identities" :key="identity.id" :value="identity.id">{{ identity.display_name }} · {{ identity.email_address }}</option></select></label>
            <label>状态<select v-model="historyFilters.status"><option value="">全部状态</option><option v-for="status in ['queued','processing','retry_wait','held','smtp_accepted','rejected','failed','outcome_unknown','expired']" :key="status" :value="status">{{ status }}</option></select></label>
            <div class="filter-action"><button class="primary-button" :disabled="busy" @click="applyHistoryFilters">筛选</button></div>
          </div>
          <p v-if="messages.length === 0" class="empty-state history-empty">没有匹配的发送记录。</p>
          <div v-for="message in messages" :key="message.id" class="account-row history-row">
            <div class="token-summary"><div class="account-name"><span :class="['badge', `status-${message.status}`]">{{ message.status }}</span><code>{{ message.id }}</code></div><div class="account-details">{{ formatTime(message.created_at) }} · {{ message.mode }} · 身份版本 {{ message.identity_version }}</div><div class="account-details">Token {{ tokens.find(token => token.id === message.token_id)?.name || message.token_id }} · {{ identities.find(identity => identity.id === message.sender_identity_id)?.display_name || message.sender_identity_id }}<span v-if="message.last_error_code"> · 错误码 {{ message.last_error_code }}</span></div></div>
            <button class="text-button" :disabled="busy" @click="openMessage(message)">详情</button>
          </div>
          <article v-if="messageDetail" class="message-detail" aria-live="polite">
            <div class="panel-heading split-heading"><div><h3>消息详情</h3><p>{{ messageDetail.id }}</p></div><button class="text-button" @click="messageDetail = null">关闭</button></div>
            <div class="detail-grid"><div><span>状态</span><strong>{{ messageDetail.status }}</strong></div><div><span>发送方式</span><strong>{{ messageDetail.mode }}</strong></div><div><span>创建时间</span><strong>{{ formatTime(messageDetail.created_at) }}</strong></div><div><span>完成时间</span><strong>{{ formatTime(messageDetail.finished_at) }}</strong></div><div><span>发件身份版本</span><strong>{{ messageDetail.identity_version }}</strong></div><div><span>安全错误码</span><strong>{{ messageDetail.last_error_code || '—' }}</strong></div></div>
            <h4>发送尝试</h4><p v-if="messageDetail.attempts.length === 0" class="empty-state">暂无发送尝试。</p><div v-for="attempt in messageDetail.attempts" :key="attempt.id" class="audit-row"><strong>第 {{ attempt.attempt_no }} 次 · {{ attempt.result || '处理中' }}</strong><span>{{ attempt.smtp_stage || '—' }} · {{ attempt.safe_error_code || '无错误码' }}</span></div>
            <h4>生命周期事件</h4><p v-if="messageDetail.events.length === 0" class="empty-state">暂无事件。</p><div v-for="event in messageDetail.events" :key="event.id" class="audit-row"><strong>{{ event.event_type }}</strong><span>{{ event.plugin_id || '系统' }}<template v-if="event.safe_details"> · {{ event.safe_details }}</template></span></div>
          </article>
        </section>
      </template>
      <footer>PostWarden <span>·</span> 自托管邮件发送代理</footer>
    </section>
  </main>
</template>
