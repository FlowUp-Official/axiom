<script setup lang="ts">
import { ref } from 'vue'

const modules = import.meta.glob('../../../guide/*.md', {
  query: '?raw',
  import: 'default',
  eager: true,
})

const order = [
  'getting-started.md',
  'configuration.md',
  'cli.md',
  'sql-annotations.md',
  'query-functions.md',
  'codegen.md',
  'database-sync.md',
  'performance.md',
  'monorepos.md',
  'license.md',
]

const markdown = ref(
  `# Axiom Documentation

Axiom is a high-performance code generator for SQL schemas and annotated query files, built for large monorepos.

${order
  .map((file) => modules[`../../../guide/${file}`])
  .filter(Boolean)
  .join('\n\n---\n\n')}
`.trim() + '\n',
)

const copied = ref(false)

async function copyDocs() {
  const text = markdown.value
  try {
    await navigator.clipboard.writeText(text)
  } catch {
    const ta = document.createElement('textarea')
    ta.value = text
    document.body.appendChild(ta)
    ta.select()
    document.execCommand('copy')
    ta.remove()
  }
  copied.value = true
  setTimeout(() => (copied.value = false), 2000)
}
</script>

<template>
  <div class="ai-copy-wrap">
    <button
      class="ai-copy"
      type="button"
      title="Copy the full documentation as Markdown"
      @click="copyDocs"
    >
      <span class="ai-copy-icon" v-if="copied">✓</span>
      <span class="ai-copy-icon" v-else>⧉</span>
      <span>{{ copied ? 'Copied!' : 'Copy docs as Markdown' }}</span>
    </button>
  </div>
</template>

<style scoped>
.ai-copy-wrap {
  display: flex;
  justify-content: flex-end;
  margin-bottom: 0.5rem;
}
.ai-copy {
  cursor: pointer;
  display: inline-flex;
  align-items: center;
  gap: 0.4rem;
  white-space: nowrap;
  font-size: 0.8rem;
  font-weight: 600;
  line-height: 1;
  padding: 0.45rem 0.75rem;
  border-radius: 6px;
  border: 1px solid var(--vp-c-brand-1);
  background: var(--vp-c-brand-soft);
  color: var(--vp-c-brand-1);
  transition: background 0.2s, color 0.2s;
}
.ai-copy:hover {
  background: var(--vp-c-brand-1);
  color: var(--vp-c-white);
}
.ai-copy-icon {
  font-size: 1rem;
  line-height: 0;
}
</style>
