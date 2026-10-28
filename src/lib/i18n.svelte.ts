import { fr } from './locales/fr';
import { en } from './locales/en';

const locales = { fr, en };

export type Lang = keyof typeof locales;
export type Key = keyof typeof fr;

export const i18n = $state({
  lang: (navigator.language.startsWith('fr') ? 'fr' : 'en') as Lang,
});

export function t(key: Key, vars: Record<string, string | number> = {}): string {
  return locales[i18n.lang][key].replace(/\{(\w+)\}/g, (_, name) => String(vars[name] ?? ''));
}

export function formatSize(mb: number): string {
  const unit = i18n.lang === 'fr' ? ['Mo', 'Go'] : ['MB', 'GB'];
  const fmt = (n: number) => n.toLocaleString(i18n.lang, { maximumFractionDigits: 1 });
  return mb >= 1000 ? `${fmt(mb / 1000)} ${unit[1]}` : `${Math.round(mb)} ${unit[0]}`;
}

export function formatDuration(seconds: number): string {
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`;
}

export function formatAge(minutes: number): string {
  const rtf = new Intl.RelativeTimeFormat(i18n.lang, { numeric: 'auto' });
  if (minutes < 60) return rtf.format(-minutes, 'minute');
  if (minutes < 60 * 24) return rtf.format(-Math.round(minutes / 60), 'hour');
  return rtf.format(-Math.round(minutes / (60 * 24)), 'day');
}
