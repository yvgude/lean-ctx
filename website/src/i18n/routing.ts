import { locales, defaultLocale } from './config';

export function getLocaleStaticPaths() {
  return locales
    .filter((l) => l !== defaultLocale)
    .map((locale) => ({ params: { locale } }));
}
