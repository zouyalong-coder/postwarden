<script setup>
import { nextTick, onMounted, reactive, ref } from 'vue'
import PluginContainer from '../plugin-runtime/PluginContainer.vue'

const props = defineProps({
  plugin: { type: Object, required: true },
  identities: { type: Array, required: true },
  accounts: { type: Array, required: true },
})

const templates = ref([])
const editingId = ref(null)
const editorOpen = ref(false)
const deletingTemplate = ref(null)
const busy = ref(false)
const notice = ref('')
const error = ref('')
const createButton = ref(null)
const confirmDeleteButton = ref(null)
let deleteTrigger = null
function setConfirmDeleteButton(element) { confirmDeleteButton.value = element }
const emptyForm = () => ({ scenario_id: '', name: '', enabled: true, allow_caller_recipients: false, sender_identity_id: '', subject_template: '', text_template: '', html_template: '', toText: '', ccText: '', bccText: '' })
const form = reactive(emptyForm())

function csrfToken() {
  const cookie = document.cookie.split('; ').find(item => item.startsWith('postwarden_csrf='))
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

async function loadTemplates() {
  templates.value = await api('/plugins/mail-template/templates')
}

function addressText(value) { return Array.isArray(value) ? value.join(', ') : '' }
function parseAddresses(value) { return value.split(/[\n,;]+/).map(item => item.trim()).filter(Boolean) }
function resetForm() { editingId.value = null; Object.assign(form, emptyForm()) }
function createTemplate() {
  error.value = ''; notice.value = ''; deletingTemplate.value = null; resetForm(); editorOpen.value = true
}
function editTemplate(template) {
  error.value = ''; notice.value = ''; deletingTemplate.value = null; editingId.value = template.id
  Object.assign(form, { scenario_id: template.scenario_id, name: template.name, enabled: template.enabled,
    allow_caller_recipients: template.allow_caller_recipients || false,
    sender_identity_id: template.sender_identity_id || '', subject_template: template.subject_template || '',
    text_template: template.text_template || '', html_template: template.html_template || '',
    toText: addressText(template.to), ccText: addressText(template.cc), bccText: addressText(template.bcc) })
  editorOpen.value = true
}
function closeTemplateEditor() {
  if (busy.value) return
  editorOpen.value = false
  error.value = ''
  resetForm()
}
async function cancelDeleteTemplate() {
  deletingTemplate.value = null
  error.value = ''
  await nextTick()
  deleteTrigger?.focus()
  deleteTrigger = null
}

async function saveTemplate() {
  error.value = ''; notice.value = ''; busy.value = true
  const body = { scenario_id: form.scenario_id.trim(), name: form.name.trim(), enabled: form.enabled,
    allow_caller_recipients: form.allow_caller_recipients,
    sender_identity_id: form.sender_identity_id, subject_template: form.subject_template,
    text_template: form.text_template || null, html_template: form.html_template || null,
    to: parseAddresses(form.toText), cc: parseAddresses(form.ccText), bcc: parseAddresses(form.bccText) }
  try {
    if (editingId.value) { await api(`/plugins/mail-template/templates/${editingId.value}`, { method: 'PUT', body }); notice.value = '邮件模板已更新。' }
    else { await api('/plugins/mail-template/templates', { method: 'POST', body }); notice.value = '邮件模板已创建。' }
    await loadTemplates()
    busy.value = false
    editorOpen.value = false; resetForm()
  } catch (cause) { error.value = cause.message }
  finally { busy.value = false }
}

async function deleteTemplate(template, event) {
  deleteTrigger = event.currentTarget
  error.value = ''; notice.value = ''; deletingTemplate.value = template
  await nextTick()
  confirmDeleteButton.value?.focus()
}

async function confirmDeleteTemplate() {
  const template = deletingTemplate.value
  if (!template) return
  error.value = ''; notice.value = ''; busy.value = true
  try {
    await api(`/plugins/mail-template/templates/${template.id}`, { method: 'DELETE' })
    if (editingId.value === template.id) resetForm()
    deletingTemplate.value = null
    notice.value = '邮件模板已删除。'; await loadTemplates()
    await nextTick()
    createButton.value?.focus()
    deleteTrigger = null
  } catch (cause) { error.value = cause.message }
  finally { busy.value = false }
}

onMounted(async () => {
  try { await loadTemplates() } catch (cause) { error.value = cause.message }
})
</script>

<template>
  <section class="panel" aria-labelledby="templates-title">
    <div class="panel-heading split-heading"><div><h2 id="templates-title">模板列表</h2><p>按场景 ID 管理发件身份、收件人、主题和正文模板。</p></div><div class="row-actions"><span class="count">{{ templates.length }} 个模板</span><button ref="createButton" class="primary-button" type="button" :disabled="busy" @click="createTemplate">创建模板</button></div></div>
    <p v-if="notice" class="notice" role="status">{{ notice }}</p>
    <p v-if="error && !editorOpen" class="notice error" role="alert">{{ error }}</p>
    <p v-if="templates.length === 0" class="empty-state">还没有模板。创建模板后，调用请求可通过场景 ID 选择对应内容和收件人。</p>
    <div v-for="template in templates" :key="template.id">
      <div class="account-row">
        <div class="token-summary"><div class="account-name">{{ template.name }} <span :class="['badge', template.enabled ? 'enabled' : 'disabled']">{{ template.enabled ? '已启用' : '已停用' }}</span></div><div class="account-details">场景 ID：<code>{{ template.scenario_id }}</code> · 发件身份：{{ identities.find(item => item.id === template.sender_identity_id)?.display_name || template.sender_identity_id || '未固定' }}</div><div class="permission-list">收件人：{{ addressText(template.to) || '由请求提供' }}<template v-if="template.cc?.length"> · 抄送：{{ addressText(template.cc) }}</template><template v-if="template.bcc?.length"> · 密送：{{ addressText(template.bcc) }}</template></div></div>
        <div class="row-actions"><button class="text-button" :disabled="busy" @click="editTemplate(template)">编辑</button><button class="text-button danger" :disabled="busy" @click="deleteTemplate(template, $event)">删除</button></div>
      </div>
      <div v-if="deletingTemplate?.id === template.id" class="template-delete-confirm" role="group" :aria-label="`删除 ${template.name}`">
        <p>确定删除邮件模板“{{ template.name }}”（{{ template.scenario_id }}）？</p>
        <div class="row-actions"><button :ref="setConfirmDeleteButton" class="text-button danger" type="button" :disabled="busy" @click="confirmDeleteTemplate">确认删除</button><button class="text-button" type="button" :disabled="busy" @click="cancelDeleteTemplate">取消</button></div>
      </div>
    </div>
  </section>
  <section class="panel template-guide">
    <div class="panel-heading"><h2>调用方式示例</h2><p>使用 Token 调用 <code>POST /api/v1/messages</code>，并设置唯一的 <code>Idempotency-Key</code>。模板插件会读取请求中的模板信息。</p></div>
    <pre class="token-example"><code>{ "sender_identity_id": "", "to": [], "subject": "", "text": null, "plugin_id": "mail-template", "plugin_input": { "scenario_id": "order.created", "variables": { "customer_name": "林先生", "order_id": "PW-2048", "amount": "¥128.00" } } }</code></pre>
    <p class="helper-text">例如模板正文可写：您好，<code v-pre>{{customer_name}}</code>，订单 <code v-pre>{{order_id}}</code> 已创建，金额 <code v-pre>{{amount}}</code>。若模板设置了固定收件人，调用方无需知道 SMTP 配置或重复传递地址。</p>
  </section>

  <PluginContainer :visible="editorOpen" :title="editingId ? '编辑模板' : '创建模板'" @close="closeTemplateEditor">
    <form id="template-form" @submit.prevent="saveTemplate">
      <p>使用 <code v-pre>{{变量名}}</code> 在主题和正文中插入变量。收件人可用逗号、分号或换行分隔。</p>
      <p v-if="error" class="notice error" role="alert">{{ error }}</p>
      <div class="form-grid">
        <label>场景 ID<input v-model.trim="form.scenario_id" autofocus maxlength="191" required placeholder="例如：order.created" /><small>调用方在模板插件输入中传入稳定的场景标识。</small></label>
        <label>模板名称<input v-model.trim="form.name" maxlength="191" required placeholder="例如：订单创建通知" /></label>
        <label>发件身份<select v-model="form.sender_identity_id" required><option value="">请选择发件身份</option><option v-for="identity in identities.filter(item => item.enabled && !item.deleted_at && accounts.some(account => account.id === item.smtp_account_id && account.enabled))" :key="identity.id" :value="identity.id">{{ identity.display_name }} · {{ identity.email_address }}</option></select></label>
        <label class="wide">主题模板<input v-model="form.subject_template" maxlength="998" required placeholder="订单 {{order_id}} 已创建" /></label>
        <label class="wide">固定收件人（To）<textarea v-model="form.toText" rows="2" placeholder="customer@example.com, ops@example.com" /><small>仅在允许请求补充收件人时可以留空。</small></label>
        <label>固定抄送（Cc）<textarea v-model="form.ccText" rows="2" placeholder="team@example.com" /></label>
        <label>固定密送（Bcc）<textarea v-model="form.bccText" rows="2" placeholder="audit@example.com" /></label>
        <label class="wide">纯文本正文<textarea v-model="form.text_template" rows="7" placeholder="您好，{{customer_name}}：\n订单 {{order_id}} 已创建，金额 {{amount}}。" /><small>纯文本或 HTML 正文至少填写一个。</small></label>
        <label class="wide">HTML 正文（可选）<textarea v-model="form.html_template" rows="7" placeholder="<p>您好，{{customer_name}}：</p><p>订单 {{order_id}} 已创建。</p>" /><small>可选；留空时仅发送纯文本正文。</small></label>
      </div>
      <label class="checkbox"><input v-model="form.allow_caller_recipients" type="checkbox" /> 允许请求补充收件人</label>
      <p class="helper-text template-recipient-help">默认由模板固定收件人，To 至少填写一个地址。开启后，请求可在固定地址之外补充收件人；此时也可以留空 To，让请求在每次发送时提供。</p>
      <label class="checkbox"><input v-model="form.enabled" type="checkbox" /> 启用此模板</label>
      <div class="form-actions row-actions"><button class="primary-button" :disabled="busy" type="submit">{{ busy ? '保存中…' : editingId ? '保存模板' : '创建模板' }}</button><button class="text-button" type="button" :disabled="busy" @click="closeTemplateEditor">取消</button></div>
    </form>
  </PluginContainer>
</template>
