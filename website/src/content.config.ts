import { defineCollection, z } from 'astro:content';
import { glob } from 'astro/loaders';

// Detailed, user-facing journey docs. One markdown file per journey, named by the
// journey slug (see src/lib/journeys.ts). Body starts at H2 — the page title/lead
// is rendered by the journey template from the journeys SSOT.
const journeys = defineCollection({
  loader: glob({ pattern: '*.md', base: './src/content/journeys' }),
  schema: z.object({
    title: z.string().optional(),
    updated: z.string().optional(),
  }),
});

export const collections = { journeys };
