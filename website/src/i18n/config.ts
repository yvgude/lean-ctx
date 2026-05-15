export const defaultLocale = 'en' as const;

export const locales = ['en'] as const;

export type Locale = (typeof locales)[number];

export const rtlLocales: readonly Locale[] = [];

export const localeNames: Record<Locale, string> = {
  en: 'English',
};

export const localeFlags: Record<Locale, string> = {
  en: '🇬🇧',
};

export function isRtl(locale: Locale): boolean {
  return rtlLocales.includes(locale);
}

export function isValidLocale(value: string): value is Locale {
  return locales.includes(value as Locale);
}
