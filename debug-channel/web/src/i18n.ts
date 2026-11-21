import { createInstance } from 'i18next';
import { initReactI18next } from 'react-i18next';
import { en, zhCN } from './catalog';

declare module 'i18next' {
  interface CustomTypeOptions {
    defaultNS: 'translation';
    resources: { translation: typeof en };
  }
}

export type Locale = 'en' | 'zh-CN';
export type LanguagePreference = Locale | 'system';
export const languageStorageKey = 'venera.language';

export function resolveLocale(languages: readonly string[]): Locale {
  for (const language of languages) {
    if (/^en(?:-|$)/i.test(language)) return 'en';
    if (/^zh(?:$|-(?:CN|SG|Hans)(?:-|$))/i.test(language)) return 'zh-CN';
  }
  return 'en';
}

export function savedPreference(): LanguagePreference {
  try {
    const value = localStorage.getItem(languageStorageKey);
    if (value === 'en' || value === 'zh-CN') return value;
  } catch {}
  return 'system';
}

export function preferenceLocale(preference: LanguagePreference): Locale {
  return preference === 'system'
    ? resolveLocale(navigator.languages)
    : preference;
}

export function createI18n(locale = preferenceLocale(savedPreference())) {
  const instance = createInstance();
  void instance.init({
    lng: locale,
    fallbackLng: 'en',
    supportedLngs: ['en', 'zh-CN'],
    resources: { en: { translation: en }, 'zh-CN': { translation: zhCN } },
    interpolation: { escapeValue: false },
    initAsync: false,
  });
  return instance;
}

export const i18n = createI18n(preferenceLocale(savedPreference()));
initReactI18next.init(i18n);
function updateDocument() {
  document.documentElement.lang = i18n.resolvedLanguage || 'en';
  document.title = `Venera · ${i18n.t('workbench')}`;
}
i18n.on('languageChanged', updateDocument);
updateDocument();

export function setLanguagePreference(preference: LanguagePreference) {
  try {
    if (preference === 'system') localStorage.removeItem(languageStorageKey);
    else localStorage.setItem(languageStorageKey, preference);
  } catch {}
  return i18n.changeLanguage(preferenceLocale(preference));
}

export function formatDate(value: string, locale = i18n.resolvedLanguage) {
  const date = new Date(value);
  return Number.isNaN(date.getTime())
    ? value
    : new Intl.DateTimeFormat(locale, {
        dateStyle: 'medium',
        timeStyle: 'medium',
      }).format(date);
}

const enumKeys = {
  event: 'event',
  log: 'log',
  status: 'status',
  trace: 'trace',
  debug: 'debug',
  info: 'info',
  warn: 'warn',
  warning: 'warn',
  error: 'error',
  fatal: 'fatal',
  unknown: 'unknown',
  hello: 'hello',
  gap: 'gap',
  'status.get': 'statusGet',
  'system.info.get': 'systemInfoGet',
  'system.info.result': 'systemInfoResult',
  'filter.set': 'filterSet',
  'filter.reset': 'filterReset',
  'request.result': 'requestResult',
} as const;
export function enumLabel(value: string) {
  return Object.hasOwn(enumKeys, value)
    ? i18n.t(enumKeys[value as keyof typeof enumKeys])
    : value;
}
