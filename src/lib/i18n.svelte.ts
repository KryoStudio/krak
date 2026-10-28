import { fr } from './locales/fr';
import { en } from './locales/en';

const locales = { fr, en };

export type Lang = keyof typeof locales;
export type Key = keyof typeof fr;

export const systemLang: Lang = navigator.language.startsWith('fr') ? 'fr' : 'en';

export const i18n = $state({ lang: systemLang as Lang });

export const hasKey = (key: string): key is Key => key in fr;

/** Picks `<key>.one` for a singular `count` (0 and 1 in French, 1 in English) when that variant exists. */
export function t(key: Key, vars: Record<string, string | number> = {}): string {
  const table: Record<string, string> = locales[i18n.lang];
  const count = vars.count;
  const singular = typeof count === 'number' && (i18n.lang === 'fr' ? count < 2 : count === 1);
  const text = (singular && table[`${key}.one`]) || table[key];
  return text.replace(/\{(\w+)\}/g, (_, name) => String(vars[name] ?? ''));
}

export function formatSize(bytes: number): string {
  const unit = i18n.lang === 'fr' ? ['Mo', 'Go'] : ['MB', 'GB'];
  const mb = bytes / 1_000_000;
  const fmt = (n: number) => n.toLocaleString(i18n.lang, { maximumFractionDigits: 1 });
  if (mb >= 1000) return `${fmt(mb / 1000)} ${unit[1]}`;
  return `${bytes > 0 ? Math.max(1, Math.round(mb)) : 0} ${unit[0]}`;
}

export function formatDuration(seconds: number): string {
  const s = Math.round(seconds);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

export function formatAge(timestamp: number): string {
  const minutes = Math.max(0, Math.round((Date.now() - timestamp) / 60_000));
  const rtf = new Intl.RelativeTimeFormat(i18n.lang, { numeric: 'auto' });
  if (minutes < 60) return rtf.format(-minutes, 'minute');
  if (minutes < 60 * 24) return rtf.format(-Math.round(minutes / 60), 'hour');
  if (minutes < 60 * 24 * 7) return rtf.format(-Math.round(minutes / (60 * 24)), 'day');
  return new Date(timestamp).toLocaleDateString(i18n.lang, { day: 'numeric', month: 'short' });
}

/** "Ctrl+Shift+F9" → "Ctrl + Maj + F9" in French. */
export function formatHotkey(accelerator: string): string[] {
  return accelerator.split('+').map((k) => (k === 'Shift' && i18n.lang === 'fr' ? 'Maj' : k));
}
