// @ts-check
import { defineConfig } from 'astro/config';
import sitemap from '@astrojs/sitemap';
import tailwindcss from '@tailwindcss/vite';

export default defineConfig({
  site: 'https://leanctx.com',
  trailingSlash: 'always',
  redirects: {
    '/context-os/': '/how-it-works/',
    '/context-os/smart-io/': '/docs/read-modes/',
    '/context-os/intelligence/': '/how-it-works/',
    '/context-os/memory/': '/docs/memory/',
    '/context-os/governance/': '/docs/governance/',
    '/context-os/verification/': '/docs/verification/',
    '/context-os/integrations/': '/setup/',
    '/context-os/benchmark/': '/benchmark/',
    '/context-os/shared-sessions/': '/docs/multi-agent/',
    '/context-os/context-bus/': '/docs/multi-agent/',
    '/context-os/sdk/': '/docs/cli/',
    '/features/': '/',
    '/pro/': '/',
    '/checkout/': '/',
    '/login/': '/',
    '/what-is-context-engineering/': '/how-it-works/',
    '/context-governance/': '/docs/governance/',
    '/docs/concepts/read-modes/': '/docs/read-modes/',
    '/docs/concepts/shell-patterns/': '/docs/shell-patterns/',
    '/docs/concepts/multi-agent/': '/docs/multi-agent/',
    '/docs/concepts/token-savings/': '/benchmark/',
    '/docs/concepts/caching/': '/how-it-works/',
    '/docs/concepts/protocols/': '/how-it-works/',
    '/docs/team-server/': '/docs/multi-agent/',
    '/docs/guides/first-session/': '/docs/getting-started/',
    '/docs/guides/editor-integrations/': '/setup/',
    '/docs/ide-setup/': '/setup/',
    '/docs/quick-reference/': '/docs/cli/',
    '/docs/api-reference/': '/docs/cli/',
    '/docs/security/': '/docs/verification/',
    '/docs/graph/': '/how-it-works/',
    '/docs/profiles/': '/docs/governance/',
    '/docs/observability/': '/docs/cli/',
    '/compatibility/': '/setup/',
    '/cli/': '/docs/cli/',
    '/mcp-server/': '/how-it-works/',
    '/shell-hook/': '/how-it-works/',
    '/dashboard/': '/',
    '/cloud/': '/',
    '/docs/tools/[...slug]/': '/tools/',
  },
  integrations: [
    sitemap({
      filter: (page) => !page.includes('/404'),
    }),
  ],
  vite: {
    plugins: [tailwindcss()]
  }
});
