// @ts-check
import { defineConfig } from 'astro/config'
import starlight from '@astrojs/starlight'
import starlightLinksValidator from 'starlight-links-validator'
import { LOCALES, t } from './locales.mjs'

// A project page of the organisation's GitHub Pages: `oxidestack.github.io` is shared by every project of the
// organisation, so this site lives under `/docx-to-pdf/`, and every internal link carries that base. The staging
// script writes the links with it (scripts/stage.mjs, BASE), and the validator below fails a link without it. A
// release's documentation is built under its own version, `/docx-to-pdf/<x.y>/`, with SITE_BASE (site/versions/).
export default defineConfig({
  site: 'https://oxidestack.github.io',
  base: process.env.SITE_BASE || '/docx-to-pdf',
  trailingSlash: 'always',
  integrations: [
    starlight({
      title: 'docx-to-pdf',
      // The project's mark beside the title, and its small mark as the favicon (public/favicon.svg): copies of the
      // OxideStack artwork (site/README.md). The README shows the same mark file.
      logo: { src: './src/assets/docx-to-pdf-mark.svg', alt: '' },
      favicon: '/favicon.svg',
      customCss: ['./src/styles/selects.css'],
      description:
        'An HTTP service that turns office documents into PDF with LibreOffice: fast, bounded in memory, safe against hostile documents.',
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/oxidestack/docx-to-pdf' }],
      // Each staged page sets its own editUrl to its source file: the default would point at the staged copy,
      // which the next build deletes.
      editLink: { baseUrl: 'https://github.com/oxidestack/docx-to-pdf/edit/main/' },
      // English at the root, every other language under its directory (locales.mjs, site/README.md "Languages").
      defaultLocale: 'root',
      locales: LOCALES,
      sidebar: [
        {
          ...t('Getting started', 'Начало работы', '入门', 'Primeros pasos'),
          items: [
            { ...t('Introduction and quick start', 'Введение и быстрый старт', '简介与快速开始', 'Introducción e inicio rápido'), slug: 'index' },
            { ...t('Installation', 'Установка', '安装', 'Instalación'), slug: 'installation' },
            { ...t('Upgrading', 'Обновление', '升级', 'Actualización'), slug: 'upgrading' },
          ],
        },
        { ...t('API reference', 'Справочник API', 'API 参考', 'Referencia de la API'), slug: 'api' },
        { ...t('Configuration', 'Конфигурация', '配置', 'Configuración'), slug: 'configuration' },
        { ...t('Operations', 'Эксплуатация', '运维', 'Operación'), slug: 'operations' },
        { ...t('Troubleshooting', 'Устранение неполадок', '故障排除', 'Solución de problemas'), slug: 'troubleshooting' },
        {
          ...t('Migrating from moalhaddar/docx-to-pdf', 'Переход с moalhaddar/docx-to-pdf', '从 moalhaddar/docx-to-pdf 迁移', 'Migrar desde moalhaddar/docx-to-pdf'),
          slug: 'migration',
        },
        { ...t('Comparison', 'Сравнение', '对比', 'Comparación'), slug: 'comparison' },
        { ...t('Benchmarks', 'Бенчмарки', '基准测试', 'Pruebas de rendimiento'), slug: 'benchmarks' },
      ],
      plugins: [
        // A broken internal link or anchor fails the build: every page is staged from a file of the repository, so a
        // link that stops resolving is a heading or a file renamed there, and the build is where to hear of it. A
        // translation's link to a page not translated yet is to that page's English fallback in its language.
        starlightLinksValidator({ errorOnRelativeLinks: true, errorOnInvalidHashes: true, errorOnFallbackPages: false }),
      ],
    }),
  ],
})
