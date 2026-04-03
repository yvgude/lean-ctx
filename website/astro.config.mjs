// @ts-check
import { defineConfig } from 'astro/config';
import sitemap from '@astrojs/sitemap';
import tailwindcss from '@tailwindcss/vite';

export default defineConfig({
  site: 'https://leanctx.com',
  integrations: [
    sitemap({
      filter: (page) => !page.includes('/index-backup/'),
    }),
  ],
  vite: {
    plugins: [tailwindcss()]
  }
});
