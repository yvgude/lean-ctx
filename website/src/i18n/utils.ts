import type { Locale } from './config';
import en from './locales/en.json';

const translations: Record<string, string> = en;

export function getLocaleFromUrl(_url: URL): Locale {
  return 'en';
}

export function getLocalizedPath(path: string, _locale: Locale): string {
  return path.startsWith('/') ? path : `/${path}`;
}

export function removeLocalePrefix(pathname: string): string {
  return pathname;
}

export function useTranslations(_locale: Locale) {
  return (key: string, replacements?: Record<string, string>): string => {
    let value = translations[key] || key;
    if (replacements) {
      for (const [k, v] of Object.entries(replacements)) {
        value = value.replace(`{${k}}`, v);
      }
    }
    return value;
  };
}
