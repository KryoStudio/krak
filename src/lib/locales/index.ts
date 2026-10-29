// The languages Krak speaks. To add one: copy en.ts to <code>.ts, translate it, and add a line
// below (README, "Translations"). Untranslated keys show in English (type it `Partial<Strings>`
// while it's in progress).
import { en } from './en';
import { fr } from './fr';
import { es } from './es';
import { pt } from './pt';
import { de } from './de';
import { it } from './it';
import { pl } from './pl';
import { tr } from './tr';
import { ru } from './ru';
import { ja } from './ja';
import { ko } from './ko';
import { zh } from './zh';

export type Key = keyof typeof en;
export type Strings = Record<Key, string>;
/** A complete language: every key, plus the plural forms it needs beyond English's
 *  (`<key>.few`, `<key>.many`… following Intl.PluralRules). */
export type Translation = Strings & Record<string, string>;

export type Language = {
  /** In the language itself. */
  name: string;
  strings: Partial<Strings>;
  /** The texts' exact locale when the code alone is ambiguous: picks the right fonts (Simplified
   *  vs Traditional Chinese) and number/date formats. */
  locale?: string;
  /** Locale whose plural rules to follow, when the language's own don't fit the texts. */
  plurals?: string;
};

const table = {
  en: { name: 'English', strings: en },
  fr: { name: 'Français', strings: fr },
  es: { name: 'Español', strings: es },
  // Brazilian wording; Intl's "pt" rules make 0 singular ("0 selecionado"), Portugal's don't.
  pt: { name: 'Português (Brasil)', strings: pt, locale: 'pt-BR', plurals: 'pt-PT' },
  de: { name: 'Deutsch', strings: de },
  it: { name: 'Italiano', strings: it },
  pl: { name: 'Polski', strings: pl },
  tr: { name: 'Türkçe', strings: tr },
  ru: { name: 'Русский', strings: ru },
  ja: { name: '日本語', strings: ja },
  ko: { name: '한국어', strings: ko },
  zh: { name: '简体中文', strings: zh, locale: 'zh-CN' },
} satisfies Record<string, Language>;

export type Lang = keyof typeof table;

export const languages: Record<Lang, Language> = table;

/** English is the reference: every key exists there. */
export const fallback: Strings = en;
