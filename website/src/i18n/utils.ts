import { defaultLocale, isValidLocale, locales, type Locale } from './config';
import manifestJson from '../../generated/mcp-tools.json';

type FlatTranslations = Record<string, string>;

const translationCache = new Map<string, FlatTranslations>();
const MCP_EXAMPLE_TOOL_COUNT = 7;

const mcpGlobalReplacements: Record<string, string> = {
  mcpToolCount: String((manifestJson as any)?.counts?.granular ?? 0),
  unifiedToolCount: String((manifestJson as any)?.counts?.unified ?? 0),
  readModeCount: String((manifestJson as any)?.read_modes?.count ?? 0),
  mcpToolMoreCount: String(
    Math.max(0, Number((manifestJson as any)?.counts?.granular ?? 0) - MCP_EXAMPLE_TOOL_COUNT),
  ),
};

function loadTranslation(locale: Locale): FlatTranslations {
  const cached = translationCache.get(locale);
  if (cached) return cached;

  const modules = import.meta.glob('./locales/*.json', { eager: true });
  const key = `./locales/${locale}.json`;
  const mod = modules[key] as { default: FlatTranslations } | undefined;
  const data = mod?.default ?? {};
  translationCache.set(locale, data);
  return data;
}

export function useTranslations(locale: Locale) {
  const translations = loadTranslation(locale);
  const fallback = locale !== defaultLocale ? loadTranslation(defaultLocale) : translations;

  return function t(key: string, replacements?: Record<string, string>): string {
    let value = translations[key] ?? fallback[key] ?? key;

    const merged = { ...mcpGlobalReplacements, ...(replacements ?? {}) };
    for (const [placeholder, replacement] of Object.entries(merged)) {
      if (replacement === undefined) continue;
        value = value.replaceAll(`{${placeholder}}`, replacement);
    }

    return value;
  };
}

export function getLocaleFromUrl(url: URL): Locale {
  const segments = url.pathname.split('/').filter(Boolean);
  const first = segments[0];
  if (first && isValidLocale(first)) return first;
  return defaultLocale;
}

export function getLocalizedPath(path: string, locale: Locale): string {
  const cleanPath = path.startsWith('/') ? path : `/${path}`;
  if (locale === defaultLocale) return cleanPath;
  return `/${locale}${cleanPath}`;
}

export function removeLocalePrefix(path: string): string {
  const segments = path.split('/').filter(Boolean);
  if (segments[0] && isValidLocale(segments[0])) {
    segments.shift();
  }
  return '/' + segments.join('/');
}

export function validateTranslations(): string[] {
  const errors: string[] = [];
  const enKeys = new Set(Object.keys(loadTranslation('en')));

  for (const locale of locales) {
    if (locale === 'en') continue;
    const localeKeys = new Set(Object.keys(loadTranslation(locale)));

    for (const key of enKeys) {
      if (!localeKeys.has(key)) {
        errors.push(`[${locale}] missing key: ${key}`);
      }
    }
    for (const key of localeKeys) {
      if (!enKeys.has(key)) {
        errors.push(`[${locale}] extra key: ${key}`);
      }
    }
  }

  return errors;
}
