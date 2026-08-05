import DefaultTheme from 'vitepress/theme'
import { h } from 'vue'
import AiCopyDocs from './components/AiCopyDocs.vue'

export default {
  extends: DefaultTheme,
  Layout() {
    return h(DefaultTheme.Layout, null, {
      'doc-before': () => h(AiCopyDocs),
    })
  },
}
