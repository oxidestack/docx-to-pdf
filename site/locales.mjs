// The site's languages (site/README.md, "Languages"). English is the original, at the site's root; every other
// language is a directory of the site, /docx-to-pdf/<dir>/, staged from its README beside README.md and from
// docs/i18n/<dir>/ (scripts/stage.mjs). A page without a translation is shown in English there, with Starlight's note
// that it is not translated yet.
//
// A label is the language's code and its own name for itself, so a reader finds their language whatever the page's.

/** Starlight's `locales`: the directory of each language, its label and its BCP 47 tag. */
export const LOCALES = {
  root: { label: 'EN (English)', lang: 'en' },
  ru: { label: 'RU (Русский)', lang: 'ru' },
  zh: { label: 'ZH (中文)', lang: 'zh-CN' },
  es: { label: 'ES (Español)', lang: 'es' },
}

/** The translated languages' directories: every locale but the original. */
export const TRANSLATIONS = Object.keys(LOCALES).filter((dir) => dir !== 'root')

/**
 * Each language's README, beside README.md: README.<tag>.md, the usual name on GitHub. Gitee and other hosts show
 * README.zh-CN.md to a reader whose browser asks for Chinese.
 */
export const README = { ru: 'README.ru.md', zh: 'README.zh-CN.md', es: 'README.es.md' }

/** A sidebar label in every language: `t('Installation', 'Установка', '安装', 'Instalación')`. */
export function t(en, ru, zh, es) {
  return { label: en, translations: { ru, 'zh-CN': zh, es } }
}
