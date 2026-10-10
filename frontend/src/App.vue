<script setup>
import { computed, onMounted, onUnmounted, reactive, ref } from 'vue'
import MailTemplate from './plugins/MailTemplate.vue'

const pluginComponents = { 'mail-template': MailTemplate }

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
const issuedTokenIdentityIds = ref([])
const pluginEditingId = ref(null)
const pluginConfigText = ref('')
const tabs = [{ id: 'accounts', label: 'SMTP 账号' }, { id: 'tokens', label: 'API Token' }, { id: 'plugins', label: '插件' }, { id: 'history', label: '发送历史' }]
function tabFromPath() {
  const path = window.location.pathname.replace(/\/$/, '')
  if (path.startsWith('/plugins/')) return 'plugins'
  return tabs.find(tab => `/${tab.id}` === path)?.id || 'accounts'
}
const activeTab = ref(tabFromPath())
const pluginPageId = ref(window.location.pathname.match(/^\/plugins\/([^/]+)\/?$/)?.[1] || null)
const pluginPage = computed(() => plugins.value.find(plugin => plugin.id === pluginPageId.value))
const configuredAccountId = ref(null)

function selectTab(id) {
  activeTab.value = id
  pluginPageId.value = null
  const path = `/${id}`
  if (window.location.pathname !== path) {
    const url = new URL(window.location.href)
    url.pathname = path
    window.history.pushState(null, '', url)
  }
}

function syncTabFromPath() {
  activeTab.value = tabFromPath()
  pluginPageId.value = window.location.pathname.match(/^\/plugins\/([^/]+)\/?$/)?.[1] || null
}

function openPluginPage(plugin) {
  if (!plugin.enabled || !pluginComponents[plugin.id]) return
  pluginPageId.value = plugin.id
  activeTab.value = 'plugins'
  window.history.pushState(null, '', `/plugins/${encodeURIComponent(plugin.id)}`)
}

function moveTab(direction) {
  const index = tabs.findIndex(tab => tab.id === activeTab.value)
  selectTab(tabs[(index + direction + tabs.length) % tabs.length].id)
  document.getElementById(`tab-${activeTab.value}`)?.focus()
}

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
async function loadAdminData() {
  await Promise.all([loadAccounts(), loadIdentities(), loadTokens(), loadPlugins(), loadMessages()])
}

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
    issuedTokenIdentityIds.value = []
    configuredAccountId.value = null
    selectTab('accounts')
    cancelEdit()
    cancelIdentityEdit()
  } catch (cause) { error.value = cause.message }
  finally { busy.value = false }
}

function edit(account) {
  clearNotice()
  selectTab('accounts')
  editingId.value = account.id
  Object.assign(form, { name: account.name, host: account.host, port: account.port,
    tls_mode: account.tls_mode, auth_type: account.auth_type, username: account.username || '',
    credential: '', enabled: account.enabled })
  document.getElementById('account-form')?.scrollIntoView({ behavior: 'smooth' })
}

function cancelEdit() { editingId.value = null; Object.assign(form, emptyForm()) }

function adjustPortForTlsMode() {
  if (form.tls_mode === 'implicit' && Number(form.port) === 587) form.port = 465
  if (form.tls_mode === 'starttls' && Number(form.port) === 465) form.port = 587
}

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
function testResultTitle(result) {
  if (result.outcome === 'accepted') return 'SMTP 服务器已接收这封邮件'
  if (result.outcome === 'outcome_unknown') return '发送结果不确定'
  if (result.outcome === 'transient_rejection' || result.outcome === 'permanent_rejection') return 'SMTP 服务器拒绝了这封邮件'
  if (result.outcome === 'connected') return 'SMTP 连接成功'
  if (result.outcome === 'connection_failed') return '无法连接 SMTP 服务器'
  return result.message
}
function testResultGuidance(result) {
  if (result.outcome === 'accepted') return '这表示邮件已提交给 SMTP 服务商，不代表目标邮箱已收到。请核对完整收件地址，检查垃圾箱/隔离区和发件邮箱的已发送/退信文件夹，稍候再查。'
  if (result.outcome === 'outcome_unknown') return '连接中断时服务端可能已经接收邮件。请先检查发件邮箱的已发送/退信文件夹和目标邮箱，再决定是否重试，以免重复发送。'
  if (result.outcome === 'transient_rejection' || result.outcome === 'permanent_rejection') return '请核对发件地址是否获准通过此 SMTP 账号发送、账号认证信息及服务商发送限额；也可查看服务商的邮件日志或退信说明。'
  if (result.outcome === 'connected') return '这里只确认可以连接 SMTP 服务；它不验证收件地址，也不代表邮件已发送或投递。'
  if (result.outcome === 'connection_failed') return '请核对主机地址、端口和 TLS 方式是否匹配（常见组合为隐式 TLS + 465 或 STARTTLS + 587），并确认本机网络可访问该服务器。'
  return ''
}
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
    if (configuredAccountId.value === account.id) {
      configuredAccountId.value = null
      cancelIdentityEdit()
    }
    notice.value = '账号已删除。'
    await Promise.all([loadAccounts(), loadIdentities(), loadTokens()])
  } catch (cause) { error.value = cause.message }
  finally { busy.value = false }
}

function accountIdentities(account) {
  return identities.value.filter(identity => identity.smtp_account_id === account.id)
}
function openAccountConfig(account) {
  if (configuredAccountId.value === account.id) {
    configuredAccountId.value = null
    cancelIdentityEdit()
  } else {
    configuredAccountId.value = account.id
    cancelIdentityEdit()
    identityForm.smtp_account_id = account.id
  }
}
function editIdentity(identity) {
  clearNotice()
  configuredAccountId.value = identity.smtp_account_id
  identityEditingId.value = identity.id
  Object.assign(identityForm, { smtp_account_id: identity.smtp_account_id, display_name: identity.display_name,
    email_address: identity.email_address,
    envelope_from: identity.envelope_from === identity.email_address ? '' : identity.envelope_from,
    enabled: identity.enabled })
  document.getElementById(`identity-form-${identity.smtp_account_id}`)?.scrollIntoView({ behavior: 'smooth' })
}
function cancelIdentityEdit() {
  identityEditingId.value = null
  Object.assign(identityForm, emptyIdentityForm(), { smtp_account_id: configuredAccountId.value || '' })
}
async function saveIdentity() {
  clearNotice(); busy.value = true
  const body = { ...identityForm, display_name: identityForm.display_name.trim(),
    email_address: identityForm.email_address.trim(),
    envelope_from: identityForm.envelope_from.trim() }
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
      issuedTokenIdentityIds.value = [...body.sender_identity_ids]
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
function clearIssuedToken() {
  issuedTokenSecret.value = ''
  issuedTokenIdentityIds.value = []
}
function sendExample(token = '<API_TOKEN>', identityId = '<SENDER_IDENTITY_ID>') {
  const payload = JSON.stringify({
    sender_identity_id: identityId,
    to: ['recipient@example.com'],
    subject: '测试通知',
    text: '这是一封测试邮件。',
  }, null, 2)
  return [
    `curl -X POST '${window.location.origin}/api/v1/messages'`,
    `  -H 'Authorization: Bearer ${token}'`,
    "  -H 'Content-Type: application/json'",
    "  -H 'Idempotency-Key: replace-with-unique-request-id'",
    `  -d '${payload}'`,
  ].join(' \\\n')
}
async function copySendExample() {
  try { await navigator.clipboard.writeText(sendExample()); notice.value = '调用示例已复制。' }
  catch { error.value = '无法访问剪贴板，请手动复制调用示例。' }
}
async function copyTokenHandoff() {
  const identities = issuedTokenIdentityIds.value.join('、')
  const handoff = `PostWarden API 调用信息\n服务地址：${window.location.origin}\nAPI Token：${issuedTokenSecret.value}\n允许的发件身份 ID：${identities}\n\n异步发送示例（替换收件人、内容和每次请求的幂等键）：\n${sendExample(issuedTokenSecret.value, issuedTokenIdentityIds.value[0])}\n\n发送成功受理后，使用返回的 status_url 查询最终状态。重试同一请求时沿用原 Idempotency-Key。`
  try { await navigator.clipboard.writeText(handoff); notice.value = '调用信息已复制，可安全地交给调用者。' }
  catch { error.value = '无法访问剪贴板，请手动复制密钥和调用示例。' }
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
async function resolveHeld(message, action) {
  const verb = action === 'resume' ? '继续处理并可能发送' : '拒绝'
  if (!window.confirm(`${verb}暂停消息 ${message.id}？`)) return
  clearNotice(); busy.value = true
  try {
    await api(`/messages/${message.id}/${action}`, { method: 'POST' })
    notice.value = action === 'resume' ? '已请求继续处理，刷新记录查看结果。' : '暂停消息已拒绝。'
    await loadMessages()
    if (messageDetail.value?.id === message.id) messageDetail.value = await api(`/messages/${message.id}`)
  } catch (cause) { error.value = cause.message }
  finally { busy.value = false }
}
function formatTime(value) { return value ? new Date(value).toLocaleString() : '—' }

onMounted(() => {
  window.addEventListener('popstate', syncTabFromPath)
  loadSession()
})
onUnmounted(() => window.removeEventListener('popstate', syncTabFromPath))
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
      <p class="intro">管理 SMTP 账号、访问 Token、构建时插件和发送历史。发件身份在所属账号的配置中管理。</p>
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
        <nav class="admin-tabs" role="tablist" aria-label="管理分类">
          <button v-for="tab in tabs" :id="`tab-${tab.id}`" :key="tab.id" type="button" role="tab" class="admin-tab" :class="{ active: activeTab === tab.id }" :aria-selected="activeTab === tab.id" :aria-controls="`panel-${tab.id}`" :tabindex="activeTab === tab.id ? 0 : -1" @click="selectTab(tab.id)" @keydown.left.prevent="moveTab(-1)" @keydown.right.prevent="moveTab(1)">{{ tab.label }}</button>
        </nav>
        <div v-show="activeTab === 'accounts'" id="panel-accounts" class="tab-content" role="tabpanel" aria-labelledby="tab-accounts">
        <section class="panel" aria-labelledby="accounts-title">
          <div class="panel-heading split-heading"><div><h2 id="accounts-title">SMTP 账号</h2><p>页面不会回显已保存的密码。</p></div><span class="count">{{ accounts.length }} 个账号</span></div>
          <p v-if="accounts.length === 0" class="empty-state">还没有 SMTP 账号。使用下方表单添加第一个发送通道。</p>
          <div v-for="account in accounts" :key="account.id" class="account-item">
            <div class="account-row">
              <div><div class="account-name">{{ account.name }} <span :class="['badge', account.enabled ? 'enabled' : 'disabled']">{{ account.enabled ? '已启用' : '已停用' }}</span></div><div class="account-details">{{ account.host }}:{{ account.port }} · {{ account.tls_mode }} · {{ account.username || '无需认证' }} · {{ accountIdentities(account).filter(item => !item.deleted_at).length }} 个发件身份</div></div>
              <div class="row-actions"><button class="text-button" type="button" :aria-expanded="configuredAccountId === account.id" :aria-controls="`account-config-${account.id}`" @click="openAccountConfig(account)">{{ configuredAccountId === account.id ? '收起配置' : '管理配置' }}</button><button class="text-button" :disabled="busy" @click="edit(account)">编辑连接</button><button class="text-button" :disabled="busy" @click="toggleAccount(account)">{{ account.enabled ? '停用' : '启用' }}</button><button class="text-button danger" :disabled="busy" @click="deleteAccount(account)">删除</button></div>
            </div>
            <div v-show="configuredAccountId === account.id" :id="`account-config-${account.id}`" class="account-config">
              <div class="subsection-heading"><div><h3>发件身份</h3><p>这些身份只使用「{{ account.name }}」发送。Token 只能选择已启用账号下的已启用身份。</p></div><span class="count">{{ accountIdentities(account).filter(item => !item.deleted_at).length }} 个</span></div>
              <p v-if="accountIdentities(account).length === 0" class="empty-state">此账号还没有发件身份。</p>
              <div v-for="identity in accountIdentities(account)" :key="identity.id" class="account-row identity-row" :class="{ 'muted-row': identity.deleted_at }">
                <div><div class="account-name">{{ identity.display_name }} &lt;{{ identity.email_address }}&gt; <span :class="['badge', identity.deleted_at ? 'disabled' : identity.enabled ? 'enabled' : 'disabled']">{{ identity.deleted_at ? '已删除' : identity.enabled ? '已启用' : '已停用' }}</span></div><div class="account-details">Envelope From: {{ identity.envelope_from }} · 版本 {{ identity.version }}</div></div>
                <div v-if="!identity.deleted_at" class="row-actions"><button class="text-button" :disabled="busy" @click="editIdentity(identity)">编辑</button><button class="text-button" :disabled="busy" @click="toggleIdentity(identity)">{{ identity.enabled ? '停用' : '启用' }}</button><button class="text-button danger" :disabled="busy" @click="deleteIdentity(identity)">删除</button></div>
              </div>
              <form :id="`identity-form-${account.id}`" class="nested-form" @submit.prevent="saveIdentity">
                <div class="subsection-heading"><div><h3>{{ identityEditingId ? '编辑发件身份' : '添加发件身份' }}</h3><p>绑定到「{{ account.name }}」。邮箱地址和信封发件地址只接受单个邮箱格式。</p></div><button v-if="identityEditingId" class="text-button" type="button" @click="cancelIdentityEdit">取消编辑</button></div>
                <div class="form-grid"><label>显示名称<input v-model="identityForm.display_name" maxlength="191" required placeholder="例如：PostWarden 通知" /></label><label>发件邮箱<input v-model="identityForm.email_address" type="email" maxlength="320" required placeholder="mailer@example.com" /></label><label>Envelope From（退信地址，可选）<input v-model="identityForm.envelope_from" type="email" maxlength="320" placeholder="留空则使用发件邮箱" /><small>留空时后台使用发件邮箱作为退信地址；需要单独接收退信时可填写其他获准使用的邮箱地址。</small></label></div>
                <label class="checkbox"><input v-model="identityForm.enabled" type="checkbox" /> 启用此身份</label>
                <div class="form-actions"><button class="primary-button" :disabled="busy || !account.enabled" type="submit">{{ busy ? '保存中…' : identityEditingId ? '保存身份' : '创建身份' }}</button><span v-if="!account.enabled" class="helper-text">先启用账号，再添加或编辑发件身份。</span></div>
              </form>
              <div class="nested-form"><div class="subsection-heading"><div><h3>连接与发信测试</h3><p>先测试 SMTP 连接，再向指定地址发送固定内容的测试邮件。</p></div><button class="text-button" :disabled="busy" @click="testConnection(account)">{{ testingAccountId === account.id ? '测试中…' : '测试连接' }}</button></div>
                <div v-if="testForm(account).result" class="test-result" :class="testForm(account).result.success ? 'test-success' : 'test-failure'" role="status"><strong>{{ testResultTitle(testForm(account).result) }}</strong><p v-if="testResultGuidance(testForm(account).result)">{{ testResultGuidance(testForm(account).result) }}</p></div>
                <div class="test-message-form"><label>发件地址<input v-model="testForm(account).from" type="email" placeholder="mailer@example.com" /><small>填写此账号获准发送的邮箱地址。</small></label><label>收件地址<input v-model="testForm(account).to" type="email" placeholder="you@example.com" /><small>填写要接收测试邮件的完整邮箱地址。</small></label><button class="primary-button" type="button" :disabled="busy || !testForm(account).from || !testForm(account).to" @click="sendTestMessage(account)">{{ testingAccountId === account.id ? '发送中…' : '发送固定测试邮件' }}</button><p class="helper-text">SMTP 服务商接受邮件不代表目标邮箱已收到，请检查垃圾箱、隔离区和退信。</p></div>
              </div>
            </div>
          </div>
        </section>

        <form id="account-form" class="panel" @submit.prevent="saveAccount">
          <div class="panel-heading split-heading"><div><h2>{{ editingId ? '编辑 SMTP 账号' : '添加 SMTP 账号' }}</h2><p>{{ editingId ? '留空密码会保留当前凭证。' : '填写服务器和认证信息。配置示例：飞书邮箱使用 smtp.feishu.cn、端口 465、隐式 TLS。' }}</p></div><button v-if="editingId" class="text-button" type="button" @click="cancelEdit">取消编辑</button></div>
          <div class="smtp-setup-guide">
            <strong>这些字段怎么填</strong>
            <ul>
              <li><b>隐式 TLS</b>：连接一开始就加密，也叫 SSL/TLS，通常配端口 <code>465</code>。例如飞书 SMTP：<code>smtp.feishu.cn</code> + <code>465</code> + 隐式 TLS。</li>
              <li><b>STARTTLS</b>：先建立 SMTP 连接，再升级为加密连接，通常配端口 <code>587</code>。端口和加密方式必须按服务商要求配对。</li>
              <li><b>认证方式</b>：PLAIN 和 LOGIN 是两种用户名/密码认证机制，不表示是否加密；启用 TLS 后认证信息通过加密连接发送。飞书可先试 LOGIN，若服务端提示不支持该机制再试 PLAIN。</li>
              <li><b>用户名</b>：通常填写完整邮箱地址，例如 <code>name@example.com</code>。</li>
              <li><b>密码</b>：填写邮箱服务商提供的 SMTP/客户端专用密码；飞书应填「IMAP/SMTP 密码」，不是网页登录密码。编辑时留空表示保留原密码。</li>
              <li>本服务只通过 SMTP 发信，不读取邮箱；无需配置 IMAP 地址或端口。</li>
            </ul>
          </div>
          <div class="form-grid">
            <label>账号名称<input v-model="form.name" maxlength="191" required placeholder="例如：事务邮件" /></label>
            <label>SMTP 主机<input v-model="form.host" maxlength="255" required placeholder="smtp.example.com" /><small>填写服务商提供的 SMTP 服务器地址，例如飞书邮箱为 smtp.feishu.cn。</small></label>
            <label>端口<input v-model.number="form.port" type="number" min="1" max="65535" required /><small>端口必须与加密方式对应：隐式 TLS 常用 465，STARTTLS 常用 587；以服务商说明为准。</small></label>
            <label>加密方式<select v-model="form.tls_mode" @change="adjustPortForTlsMode"><option value="starttls">STARTTLS（通常端口 587）</option><option value="implicit">隐式 TLS（通常端口 465）</option><option value="none">无 TLS（仅免认证）</option></select><small v-if="form.tls_mode === 'implicit'">连接后立即加密；飞书请使用 465。若端口原为 587，已自动改为 465。</small><small v-else-if="form.tls_mode === 'starttls'">连接建立后再升级为加密；飞书请使用 587。若端口原为 465，已自动改为 587。</small><small v-else>仅适用于明确允许免认证且无需加密的 SMTP 服务。</small></label>
            <label>认证方式<select v-model="form.auth_type"><option value="plain">PLAIN（用户名和密码）</option><option value="login">LOGIN（用户名和密码）</option><option value="none">无需认证</option></select><small>PLAIN/LOGIN 是认证协议名称，不是加密开关；按服务商支持的方式选择。大多数邮箱要求认证。</small></label>
            <label v-if="form.auth_type !== 'none'">用户名<input v-model="form.username" autocomplete="off" :required="!editingId" placeholder="name@example.com" /><small>通常填写完整邮箱地址，而不是只填 @ 前面的名称。</small></label>
            <label v-if="form.auth_type !== 'none'" class="wide">SMTP 密码<input v-model="form.credential" type="password" autocomplete="new-password" :required="!editingId" /><small>填写服务商提供的 SMTP/客户端专用密码。若服务商要求授权码，请填授权码；不要在此处填写 IMAP 服务器信息。</small></label>
          </div>
          <label class="checkbox"><input v-model="form.enabled" type="checkbox" /> 启用此账号</label>
          <div class="form-actions"><button class="primary-button" :disabled="busy" type="submit">{{ busy ? '保存中…' : editingId ? '保存更改' : '创建账号' }}</button></div>
        </form>

        </div>
        <div v-show="activeTab === 'tokens'" id="panel-tokens" class="tab-content" role="tabpanel" aria-labelledby="tab-tokens">
        <section v-if="issuedTokenSecret" class="panel secret-panel" aria-live="polite">
          <div class="panel-heading"><h2>新 Token 密钥</h2><p>此密钥只显示这一次。请现在复制并妥善保存，离开此页面后无法再次查看。</p></div>
          <div class="secret-value"><code>{{ issuedTokenSecret }}</code><div class="secret-actions"><button class="primary-button" type="button" @click="copyTokenHandoff">复制调用信息</button><button class="text-button" type="button" @click="copyTokenSecret">仅复制密钥</button><button class="text-button" type="button" @click="clearIssuedToken">我已保存</button></div></div>
          <p class="handoff-note">调用信息包含密钥、服务地址、允许的发件身份 ID 和请求示例。请通过可信渠道交给调用者。</p>
        </section>

        <section class="panel" aria-labelledby="token-guide-title">
          <div class="panel-heading split-heading"><div><h2 id="token-guide-title">如何使用 API Token</h2><p>调用者使用 HTTPS 请求发送接口；Token 放在 Authorization 请求头中。</p></div><button class="text-button" type="button" @click="copySendExample">复制示例</button></div>
          <ol class="token-guide-steps">
            <li>创建 Token 并授权至少一个发件身份，将创建时显示的调用信息交给调用者。</li>
            <li>发送请求时填入 Token 和已授权的 <code>sender_identity_id</code>，为每次发送指定唯一的 <code>Idempotency-Key</code>。</li>
            <li>异步接口返回 <code>202</code> 后，使用响应中的 <code>status_url</code> 查询最终状态。重试同一请求时沿用原幂等键。</li>
          </ol>
          <pre class="token-example"><code>{{ sendExample() }}</code></pre>
          <p class="helper-text">示例中的 Token、身份 ID、收件人、内容和幂等键都需要替换。已有 Token 的密钥无法再次查看。</p>
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

        </div>
        <div v-show="activeTab === 'plugins'" id="panel-plugins" class="tab-content" role="tabpanel" aria-labelledby="tab-plugins">
        <section v-if="!pluginPageId" class="panel" aria-labelledby="plugins-title">
          <div class="panel-heading split-heading"><div><h2 id="plugins-title">构建时插件</h2><p>插件配置在服务端加密保存。这里不会回显配置内容。</p></div><span class="count">{{ plugins.length }} 个插件</span></div>
          <p v-if="plugins.length === 0" class="empty-state">当前构建没有注册插件。</p>
          <div v-for="plugin in plugins" :key="plugin.id" class="plugin-entry">
          <div class="account-row plugin-row">
            <div class="token-summary"><div class="account-name">{{ plugin.id }} <span :class="['badge', plugin.enabled ? 'enabled' : 'disabled']">{{ plugin.enabled ? '已启用' : '已停用' }}</span></div><div class="account-details">版本 {{ plugin.version }} · 执行顺序 {{ plugin.execution_order }} · {{ plugin.has_config ? '已有加密配置' : '使用默认配置' }}</div>
              <div v-if="pluginEditingId === plugin.id" class="plugin-editor"><label>完整 JSON 配置<textarea v-model="pluginConfigText" rows="6" spellcheck="false" :placeholder="plugin.has_config ? '请输入完整的新配置 JSON 对象' : '{}'" required></textarea><small>出于安全原因，已有配置不会回显。保存时会用此 JSON 完整替换原配置。</small></label><div class="row-actions"><button class="primary-button" :disabled="busy || !pluginConfigText.trim()" @click="savePlugin(plugin)">保存设置</button><button class="text-button" :disabled="busy" @click="cancelPluginEdit">取消</button></div></div>
            </div>
            <div v-if="pluginEditingId !== plugin.id" class="row-actions"><button v-if="pluginComponents[plugin.id]" class="text-button" :disabled="busy || !plugin.enabled" @click="openPluginPage(plugin)">管理</button><button class="text-button" :disabled="busy || !plugin.enabled" @click="editPlugin(plugin)">配置</button><button class="text-button" :disabled="busy" @click="togglePlugin(plugin)">{{ plugin.enabled ? '停用' : '启用' }}</button></div>
          </div>
          </div>
        </section>
        <div v-else class="plugin-page">
          <button class="text-button plugin-back" type="button" @click="selectTab('plugins')">← 返回插件列表</button>
          <template v-if="pluginPage && pluginPage.enabled && pluginComponents[pluginPage.id]">
            <component :is="pluginComponents[pluginPage.id]" :plugin="pluginPage" :identities="identities" :accounts="accounts" />
          </template>
          <section v-else class="panel" aria-label="插件管理不可用">
            <h2>{{ pluginPage?.id || pluginPageId }}</h2>
            <p class="helper-text">{{ pluginPage?.enabled === false ? '插件已停用，启用后才能管理。' : '此插件没有可用的管理页面。' }}</p>
          </section>
        </div>

        </div>
        <div v-show="activeTab === 'history'" id="panel-history" class="tab-content" role="tabpanel" aria-labelledby="tab-history">
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
            <div class="row-actions"><button v-if="message.status === 'held'" class="text-button" :disabled="busy" @click="resolveHeld(message, 'resume')">继续</button><button v-if="message.status === 'held'" class="text-button danger" :disabled="busy" @click="resolveHeld(message, 'reject')">拒绝</button><button class="text-button" :disabled="busy" @click="openMessage(message)">详情</button></div>
          </div>
          <article v-if="messageDetail" class="message-detail" aria-live="polite">
            <div class="panel-heading split-heading"><div><h3>消息详情</h3><p>{{ messageDetail.id }}</p></div><button class="text-button" @click="messageDetail = null">关闭</button></div>
            <div class="detail-grid"><div><span>状态</span><strong>{{ messageDetail.status }}</strong></div><div><span>发送方式</span><strong>{{ messageDetail.mode }}</strong></div><div><span>创建时间</span><strong>{{ formatTime(messageDetail.created_at) }}</strong></div><div><span>完成时间</span><strong>{{ formatTime(messageDetail.finished_at) }}</strong></div><div><span>发件身份版本</span><strong>{{ messageDetail.identity_version }}</strong></div><div><span>安全错误码</span><strong>{{ messageDetail.last_error_code || '—' }}</strong></div></div>
            <h4>发送尝试</h4><p v-if="messageDetail.attempts.length === 0" class="empty-state">暂无发送尝试。</p><div v-for="attempt in messageDetail.attempts" :key="attempt.id" class="audit-row"><strong>第 {{ attempt.attempt_no }} 次 · {{ attempt.result || '处理中' }}</strong><span>{{ attempt.smtp_stage || '—' }} · {{ attempt.safe_error_code || '无错误码' }}</span></div>
            <h4>生命周期事件</h4><p v-if="messageDetail.events.length === 0" class="empty-state">暂无事件。</p><div v-for="event in messageDetail.events" :key="event.id" class="audit-row"><strong>{{ event.event_type }}</strong><span>{{ event.plugin_id || '系统' }}<template v-if="event.safe_details"> · {{ event.safe_details }}</template></span></div>
          </article>
        </section>
        </div>
      </template>
      <footer>PostWarden <span>·</span> 自托管邮件发送代理</footer>
    </section>
  </main>
</template>
