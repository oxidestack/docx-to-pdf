import { defineCollection } from 'astro:content'
import { docsLoader } from '@astrojs/starlight/loaders'
import { docsSchema } from '@astrojs/starlight/schema'

// The pages are staged into src/content/docs/ by scripts/stage.mjs on every build; none is written here.
export const collections = {
  docs: defineCollection({ loader: docsLoader(), schema: docsSchema() }),
}
