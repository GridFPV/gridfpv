/** The help pages (`lib/help.ts`): the page list built from Markdown, and the bundled pages. */
import { describe, expect, it } from 'vitest';
import { HELP_PAGES, buildHelpPages, helpPage } from '../src/lib/help.js';

const page = (title: string, about = 'About it.') => `# ${title}\n\n${about}\n\n## More\n\nRest.`;

describe('buildHelpPages', () => {
  it('makes a page from each file: its name in the URL, its title, and what it is about', () => {
    const markdown =
      '# Run a heat\n\nPut a **heat** on the timer, then\nstart.\n\n## Keys\n\nSpace.';
    expect(buildHelpPages({ '../../../../../docs/help/run.md': markdown })).toEqual([
      {
        slug: 'run',
        title: 'Run a heat',
        about: 'Put a heat on the timer, then start.',
        markdown
      }
    ]);
  });

  it('takes the name from the file name, whatever folder it is in', () => {
    const pages = buildHelpPages({
      'start-here.md': page('A'),
      '/abs/docs/help/words.md': page('B'),
      './some.dir/my-page.md': page('C')
    });
    expect(pages.map((made) => made.slug)).toEqual(['start-here', 'words', 'my-page']);
  });

  it('falls back to the name for a page with no title, and to nothing for one with no words', () => {
    expect(buildHelpPages({ 'docs/help/seats.md': 'Just words.' })).toEqual([
      { slug: 'seats', title: 'seats', about: 'Just words.', markdown: 'Just words.' }
    ]);
    expect(buildHelpPages({ 'docs/help/seats.md': '' })).toEqual([
      { slug: 'seats', title: 'seats', about: '', markdown: '' }
    ]);
  });

  it('lists getting started first, then the pages in the order of the nav', () => {
    const order = [
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
    // Handed over backwards, so the order cannot come from the input.
    const sources = Object.fromEntries(
      [...order].reverse().map((slug) => [`docs/help/${slug}.md`, page(slug)])
    );
    expect(buildHelpPages(sources).map((made) => made.slug)).toEqual(order);
  });

  it('puts a page it has no place for after the others, by name', () => {
    const sources = {
      'docs/help/zebra.md': page('Z'),
      'docs/help/overlays.md': page('O'),
      'docs/help/apple.md': page('A'),
      'docs/help/start-here.md': page('S')
    };
    expect(buildHelpPages(sources).map((made) => made.slug)).toEqual([
      'start-here',
      'overlays',
      'apple',
      'zebra'
    ]);
  });

  it('is empty with no files', () => {
    expect(buildHelpPages({})).toEqual([]);
  });
});

describe('the bundled help pages', () => {
  it('include the page a new director starts on, first', () => {
    expect(HELP_PAGES.length).toBeGreaterThan(0);
    expect(HELP_PAGES[0].slug).toBe('start-here');
  });

  it('each have a title of their own and say what they are about', () => {
    for (const made of HELP_PAGES) {
      expect(made.markdown, made.slug).toMatch(/^# \S/m);
      expect(made.title, made.slug).not.toBe(made.slug);
      expect(made.about, made.slug).not.toBe('');
    }
  });

  it('each have a name of their own', () => {
    const slugs = HELP_PAGES.map((made) => made.slug);
    expect(new Set(slugs).size).toBe(slugs.length);
  });

  it('helpPage finds a page by its name, and nothing for a name there is no page for', () => {
    expect(helpPage('start-here')).toBe(HELP_PAGES[0]);
    expect(helpPage('no-such-page')).toBeUndefined();
    expect(helpPage('')).toBeUndefined();
  });

  it('link only to console pages and the web, so no link is sent to the index by mistake', () => {
    for (const made of HELP_PAGES) {
      const links = [...made.markdown.matchAll(/\[[^\]]+\]\(([^)\s]+)\)/g)].map((m) => m[1]);
      for (const link of links) {
        expect(link, `${made.slug}: ${link}`).toMatch(/^(#\/|https?:\/\/)/);
      }
    }
  });

  it('link only to help pages that exist', () => {
    for (const made of HELP_PAGES) {
      const slugs = [...made.markdown.matchAll(/\]\(#\/help\/([^)\s]+)\)/g)].map((m) => m[1]);
      for (const slug of slugs) {
        expect(helpPage(slug), `${made.slug} links to #/help/${slug}`).toBeDefined();
      }
    }
  });
});
