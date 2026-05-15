export const defaultLocale = 'en' as const;

export const locales = ['en', 'de', 'ar', 'zh', 'hi', 'es', 'fr', 'bn', 'pt', 'ru', 'ja'] as const;

export type Locale = (typeof locales)[number];

export const rtlLocales: readonly Locale[] = ['ar'];

export const localeNames: Record<Locale, string> = {
  en: 'English',
  de: 'Deutsch',
  ar: 'العربية',
  zh: '中文',
  hi: 'हिन्दी',
  es: 'Español',
  fr: 'Français',
  bn: 'বাংলা',
  pt: 'Português',
  ru: 'Русский',
  ja: '日本語',
};

export const localeFlags: Record<Locale, string> = {
  en: '🇬🇧',
  de: '🇩🇪',
  ar: '🇸🇦',
  zh: '🇨🇳',
  hi: '🇮🇳',
  es: '🇪🇸',
  fr: '🇫🇷',
  bn: '🇧🇩',
  pt: '🇧🇷',
  ru: '🇷🇺',
  ja: '🇯🇵',
};

export function isRtl(locale: Locale): boolean {
  return rtlLocales.includes(locale);
}

export function isValidLocale(value: string): value is Locale {
  return locales.includes(value as Locale);
}
