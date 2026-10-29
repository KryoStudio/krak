import { fallback, languages, type Key, type Lang } from './locales';

export type { Key, Lang };

/** The first of the PC's preferred languages that Krak speaks, else English. */
function detect(): Lang {
  for (const tag of navigator.languages?.length ? navigator.languages : [navigator.language]) {
    const base = tag.toLowerCase().split('-')[0];
    if (Object.hasOwn(languages, base)) return base as Lang;
  }
  return 'en';
}

export const systemLang: Lang = detect();

export const i18n = $state({ lang: systemLang as Lang });

/** A saved language that this version doesn't have (removed, or from a newer version) → system. */
export const resolveLang = (code: string | null | undefined): Lang =>
  code && Object.hasOwn(languages, code) ? (code as Lang) : systemLang;

/** BCP 47 locale of the current language, for `lang` and Intl formats. */
export const locale = (): string => languages[i18n.lang].locale ?? i18n.lang;

export const hasKey = (key: string): key is Key => key in fallback;

/**
 * Translates `key`, filling `{name}` placeholders. With a numeric `count`, picks the plural
 * variant of the language (`<key>.one`, `<key>.few`…) when it exists. Missing → English.
 */
export function t(key: Key, vars: Record<string, string | number> = {}): string {
  const language = languages[i18n.lang];
  const table: Record<string, string | undefined> = language.strings;
  const base: Record<string, string> = fallback;
  let id: string = key;
  if (typeof vars.count === 'number') {
    const category = new Intl.PluralRules(language.plurals ?? locale()).select(vars.count);
    if (category !== 'other' && (table[`${key}.${category}`] ?? base[`${key}.${category}`])) id = `${key}.${category}`;
  }
  const text = table[id] ?? base[id] ?? key;
  return text.replace(/\{(\w+)\}/g, (_, name) => String(vars[name] ?? ''));
}

/** Bytes per MB and GB as Windows counts them, so sizes match File Explorer (and the backend's
 *  storage limit). */
export const MB = 1024 ** 2;
export const GB = 1024 ** 3;

export function formatSize(bytes: number): string {
  const mb = bytes / MB;
  const fmt = (n: number) => n.toLocaleString(locale(), { maximumFractionDigits: 1 });
  if (mb >= 1000) return `${fmt(bytes / GB)} ${t('unit.gb')}`;
  return `${bytes > 0 ? Math.max(1, Math.round(mb)) : 0} ${t('unit.mb')}`;
}

export function formatDuration(seconds: number): string {
  const s = Math.round(seconds);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

export function formatAge(timestamp: number): string {
  const minutes = Math.max(0, Math.round((Date.now() - timestamp) / 60_000));
  const rtf = new Intl.RelativeTimeFormat(locale(), { numeric: 'auto' });
  if (minutes < 60) return rtf.format(-minutes, 'minute');
  if (minutes < 60 * 24) return rtf.format(-Math.round(minutes / 60), 'hour');
  if (minutes < 60 * 24 * 7) return rtf.format(-Math.round(minutes / (60 * 24)), 'day');
  return new Date(timestamp).toLocaleDateString(locale(), { day: 'numeric', month: 'short' });
}

/** "Ctrl+Shift+F9" → ["Ctrl", "Maj", "F9"] in French. */
export function formatHotkey(accelerator: string): string[] {
  return accelerator.split('+').map((k) => (k === 'Shift' ? t('key.shift') : k));
}
