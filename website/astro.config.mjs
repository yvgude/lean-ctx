// @ts-check
import { defineConfig } from 'astro/config';
import sitemap from '@astrojs/sitemap';
import tailwindcss from '@tailwindcss/vite';

export default defineConfig({
  site: 'https://leanctx.com',
  i18n: {
    defaultLocale: 'en',
    locales: ['en', 'de', 'ar', 'zh', 'hi', 'es', 'fr', 'bn', 'pt', 'ru', 'ja'],
    routing: {
      prefixDefaultLocale: false,
    },
  },
  integrations: [
    sitemap({
      filter: (page) => !page.includes('/index-backup/'),
      i18n: {
        defaultLocale: 'en',
        locales: {
          en: 'en',
          de: 'de',
          ar: 'ar',
          zh: 'zh',
          hi: 'hi',
          es: 'es',
          fr: 'fr',
          bn: 'bn',
          pt: 'pt',
          ru: 'ru',
          ja: 'ja',
        },
      },
    }),
  ],
  vite: {
    plugins: [tailwindcss()]
  }
});
