import { defineConfig } from 'vitepress'

export default defineConfig({
  title: 'Axiom',
  description:
    'High-performance code generator for SQL schemas and queries, built for large monorepos.',
  lang: 'en-US',
  base: '/axiom/',
  cleanUrls: true,
  lastUpdated: true,

  themeConfig: {
    nav: [
      { text: 'Guide', link: '/guide/getting-started' },
      { text: 'CLI', link: '/guide/cli' },
      { text: 'GitHub', link: 'https://github.com/FlowUp-Official/axiom' },
    ],

    sidebar: {
      '/guide/': [
        {
          text: 'Guide',
          items: [
            { text: 'Getting Started', link: '/guide/getting-started' },
            { text: 'Configuration', link: '/guide/configuration' },
            { text: 'CLI Reference', link: '/guide/cli' },
            { text: 'Check', link: '/guide/check' },
            { text: 'Format', link: '/guide/format' },
            { text: 'Lint', link: '/guide/lint' },
            { text: 'SQL Annotations', link: '/guide/sql-annotations' },
            { text: 'Query Functions', link: '/guide/query-functions' },
            { text: 'Code Generation', link: '/guide/codegen' },
            { text: 'Database Sync', link: '/guide/database-sync' },
            { text: 'Performance', link: '/guide/performance' },
            { text: 'Monorepos', link: '/guide/monorepos' },
            { text: 'License', link: '/guide/license' },
          ],
        },
      ],
    },

    footer: {
      message: 'Licensed under the Apache License, Version 2.0',
    },

    socialLinks: [
      { icon: 'github', link: 'https://github.com/FlowUp-Official/axiom' },
    ],
  },
})
