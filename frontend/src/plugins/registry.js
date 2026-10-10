import MailTemplate from './MailTemplate.vue'

// Frontend capabilities are registered here so App.vue does not need plugin IDs.
// A custom configComponent receives { plugin, busy } and emits `save` with a
// config object, plus `cancel` when the editor should close.
export const pluginRegistry = {
  'mail-template': {
    managementComponent: MailTemplate,
  },
}

export function pluginDescriptor(pluginId) {
  return pluginRegistry[pluginId] || {}
}

export function hasPluginManagementPage(pluginId) {
  return Boolean(pluginDescriptor(pluginId).managementComponent)
}
