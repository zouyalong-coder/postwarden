import MailTemplate from './MailTemplate.vue'

// Frontend capabilities are registered here so App.vue does not need plugin IDs.
// A custom configComponent receives { plugin, busy } and emits `save` with a
// config object, plus `cancel` when the editor should close.
export const pluginRegistry = {
  'mail-template': {
    managementComponent: MailTemplate,
    managementTitle: '邮件模板插件',
    callExample: {
      label: '邮件模板',
      description: '模板会填入留空的发件身份、收件人、主题和正文。替换场景 ID、变量和幂等键；Token 须获准使用模板绑定的发件身份。',
      fields: { sender_identity_id: '', to: [], subject: '', text: null },
      input: {
        scenario_id: '<SCENARIO_ID>',
        variables: { customer_name: '林先生', order_id: 'PW-2048' },
      },
    },
  },
}

export function pluginDescriptor(pluginId) {
  return pluginRegistry[pluginId] || {}
}

export function hasPluginManagementPage(pluginId) {
  return Boolean(pluginDescriptor(pluginId).managementComponent)
}
