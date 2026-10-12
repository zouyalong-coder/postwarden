<script>
const dialogStack = []
let lockedBodyOverflow = ''
let lockedAppInert = false
</script>

<script setup>
import { nextTick, onBeforeUnmount, ref, watch } from 'vue'

const props = defineProps({
  title: { type: String, required: true },
  visible: { type: Boolean, default: false },
})

const emit = defineEmits(['close'])
const titleId = `plugin-dialog-title-${Math.random().toString(36).slice(2)}`
const backdrop = ref(null)
const dialog = ref(null)
const isTopDialog = ref(false)
const depth = ref(0)
let stackEntry = null

const focusableSelector = [
  'a[href]',
  'button:not([disabled])',
  'input:not([disabled]):not([type="hidden"])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  '[tabindex]:not([tabindex="-1"])',
].join(',')

function syncDialogStack() {
  dialogStack.forEach((entry, index) => {
    const top = index === dialogStack.length - 1
    entry.isTop.value = top
    entry.depth.value = index
    if (entry.backdrop.value) {
      entry.backdrop.value.style.zIndex = String(1000 + index * 20)
    }
    if (entry.dialog.value) {
      entry.dialog.value.inert = !top
      if (top) entry.dialog.value.setAttribute('aria-modal', 'true')
      else entry.dialog.value.removeAttribute('aria-modal')
    }
  })
}

function lockPageForDialogs() {
  if (dialogStack.length !== 1) return
  lockedBodyOverflow = document.body.style.overflow
  document.body.style.overflow = 'hidden'
  const app = document.getElementById('app')
  if (app) {
    lockedAppInert = app.inert
    app.inert = true
  }
}

function unlockPageAfterDialogs() {
  if (dialogStack.length) return
  document.body.style.overflow = lockedBodyOverflow
  const app = document.getElementById('app')
  if (app) app.inert = lockedAppInert
}

function openDialog() {
  if (stackEntry) return
  stackEntry = {
    backdrop,
    dialog,
    isTop: isTopDialog,
    depth,
    returnFocus: document.activeElement,
  }
  dialogStack.push(stackEntry)
  lockPageForDialogs()
  syncDialogStack()
}

async function closeDialog() {
  if (!stackEntry) return
  const entry = stackEntry
  const index = dialogStack.indexOf(entry)
  if (index !== -1) dialogStack.splice(index, 1)
  stackEntry = null
  syncDialogStack()
  unlockPageAfterDialogs()
  await nextTick()
  entry.returnFocus?.focus?.()
}

function focusInitialElement() {
  const targetDialog = dialog.value
  if (!targetDialog) return
  const preferred = targetDialog.querySelector('[autofocus]')
  const first = targetDialog.querySelector(focusableSelector)
  ;(preferred || first || targetDialog).focus()
}

function handleKeydown(event) {
  if (!isTopDialog.value) return
  if (event.key === 'Escape') {
    event.preventDefault()
    event.stopPropagation()
    emit('close')
    return
  }
  if (event.key !== 'Tab') return

  const targetDialog = dialog.value
  if (!targetDialog) return
  const focusable = [...targetDialog.querySelectorAll(focusableSelector)]
    .filter((element) => element.getClientRects().length > 0)
  if (!focusable.length) {
    event.preventDefault()
    targetDialog.focus()
    return
  }

  const first = focusable[0]
  const last = focusable[focusable.length - 1]
  if (event.shiftKey && (document.activeElement === first || !targetDialog.contains(document.activeElement))) {
    event.preventDefault()
    last.focus()
  } else if (!event.shiftKey && (document.activeElement === last || !targetDialog.contains(document.activeElement))) {
    event.preventDefault()
    first.focus()
  }
}

watch(() => props.visible, async (visible) => {
  if (visible) {
    openDialog()
    await nextTick()
    syncDialogStack()
    if (isTopDialog.value) focusInitialElement()
  } else {
    await closeDialog()
  }
}, { flush: 'post', immediate: true })

onBeforeUnmount(() => {
  if (stackEntry) {
    const entry = stackEntry
    const index = dialogStack.indexOf(entry)
    if (index !== -1) dialogStack.splice(index, 1)
    stackEntry = null
    syncDialogStack()
    unlockPageAfterDialogs()
    entry.returnFocus?.focus?.()
  }
})
</script>

<template>
  <Teleport to="body">
    <div v-if="visible" ref="backdrop" class="plugin-dialog-backdrop">
      <section
        ref="dialog"
        class="plugin-dialog"
        :class="{ 'plugin-dialog-nested': depth > 0 }"
        role="dialog"
        :aria-modal="isTopDialog ? 'true' : undefined"
        :aria-hidden="isTopDialog ? undefined : 'true'"
        :aria-labelledby="titleId"
        tabindex="-1"
        data-plugin-dialog
        @keydown="handleKeydown"
      >
        <header class="plugin-dialog-header">
          <h2 :id="titleId">{{ title }}</h2>
          <button class="plugin-dialog-close" type="button" aria-label="关闭" @click="emit('close')">
            <span aria-hidden="true">×</span>
          </button>
        </header>
        <div class="plugin-dialog-content">
          <slot />
        </div>
      </section>
    </div>
  </Teleport>
</template>
