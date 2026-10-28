// @ts-check
import { defineConfig } from 'astro/config'
import starlight from '@astrojs/starlight'
import starlightLinksValidator from 'starlight-links-validator'

// A project page of the organisation's GitHub Pages: `oxidestack.github.io` is shared by every project of the
// organisation, so this site lives under `/docx-to-pdf/`, and every internal link carries that base. The staging
// script writes the links with it (scripts/stage.mjs, BASE), and the validator below fails a link without it.
export default defineConfig({
  site: 'https://oxidestack.github.io',
  base: '/docx-to-pdf',
  trailingSlash: 'always',
  integrations: [
    starlight({
      title: 'docx-to-pdf',
      description:
        'An HTTP service that turns an office document into a PDF with LibreOffice, a drop-in replacement for moalhaddar/docx-to-pdf.',
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/oxidestack/docx-to-pdf' }],
      // Each staged page sets its own editUrl to its source file: the default would point at the staged copy,
      // which the next build deletes.
      editLink: { baseUrl: 'https://github.com/oxidestack/docx-to-pdf/edit/main/' },
      sidebar: [
        {
          label: 'Getting started',
          items: [
            { label: 'Introduction and quick start', link: '/' },
            { label: 'Installation', slug: 'installation' },
          ],
        },
        { label: 'API reference', slug: 'api' },
        { label: 'Configuration', slug: 'configuration' },
        { label: 'Operations', slug: 'operations' },
        { label: 'Troubleshooting', slug: 'troubleshooting' },
        { label: 'Migrating from moalhaddar/docx-to-pdf', slug: 'migration' },
        { label: 'Comparison', slug: 'comparison' },
      ],
      plugins: [
        // A broken internal link or anchor fails the build: every page is staged from a file of the repository, so a
        // link that stops resolving is a heading or a file renamed there, and the build is where to hear of it.
        starlightLinksValidator({ errorOnRelativeLinks: true, errorOnInvalidHashes: true }),
      ],
    }),
  ],
})
