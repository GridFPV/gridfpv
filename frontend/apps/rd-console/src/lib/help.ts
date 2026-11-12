/**
 * The help pages: Markdown from `docs/help/*.md`, bundled into the console at build time.
 * The same text reads on GitHub and in the console, and nothing is fetched at run time, so
 * help works at a field with no internet.
 *
 * There is one page per console page, named for it, plus a few that explain an idea
 * (`start-here`, `words`). The `?` beside a page's title opens that page's help.
 */
import { firstParagraph, titleOf } from './markdown.js';

const pages = import.meta.glob('../../../../../docs/help/*.md', {
  query: '?raw',
  import: 'default',
  eager: true
}) as Record<string, string>;

export interface HelpPage {
  /** The page's name in the URL: `#/help/‹slug›`. */
  slug: string;
  title: string;
  /** What the page is about, in a sentence or two. */
  about: string;
  markdown: string;
}

/** The order the index lists pages in: getting started, then the nav, left to right. */
const ORDER = [
  'start-here',
  'words',
  'event',
  'seats',
  'tune',
  'pilots',
  'phases',
  'formats',
  'run',
  'marshal',
  'results',
  'overlays'
];

const slugOf = (path: string): string => path.replace(/^.*\//, '').replace(/\.md$/, '');

/** Build the page list from Markdown sources by path. Exported for tests. */
export function buildHelpPages(sources: Record<string, string>): HelpPage[] {
  const rank = (slug: string): number => {
    const at = ORDER.indexOf(slug);
    return at === -1 ? ORDER.length : at;
  };
  return Object.entries(sources)
    .map(([path, markdown]) => ({
      slug: slugOf(path),
      title: titleOf(markdown, slugOf(path)),
      about: firstParagraph(markdown),
      markdown
    }))
    .sort((a, b) => rank(a.slug) - rank(b.slug) || a.slug.localeCompare(b.slug));
}

export const HELP_PAGES: HelpPage[] = buildHelpPages(pages);

export function helpPage(slug: string): HelpPage | undefined {
  return HELP_PAGES.find((page) => page.slug === slug);
}
