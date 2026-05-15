export const defaultLocale = 'en';
export const locales = ['en'] as const;
export type Locale = (typeof locales)[number];

export function isRtl(_locale: Locale): boolean {
  return false;
}
