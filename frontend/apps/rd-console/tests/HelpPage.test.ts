/**
 * The Help page: the index of every help page, one page rendered from its Markdown, and the
 * links that hold the console and its help together. The pages are the real ones from
 * `docs/help`, bundled the way the console bundles them. `help.test.ts` covers how the list
 * is built and `markdown.test.ts` the renderer.
 */
import { describe, expect, it } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import HelpPage from '../src/screens/HelpPage.svelte';
import { HELP_PAGES, helpPage } from '../src/lib/help.js';

/** Every screen's source, to read which help page each `?` opens. */
const SCREENS = import.meta.glob('../src/**/*.svelte', {
  query: '?raw',
  import: 'default',
  eager: true
}) as Record<string, string>;

describe('HelpPage index', () => {
  it('lists every help page by title with what it is about, each a link to that page', () => {
    render(HelpPage, {});
    expect(screen.getByRole('heading', { level: 1, name: 'Help' })).toBeVisible();
    const tiles = within(screen.getByRole('list', { name: 'Help pages' })).getAllByRole('link');
    expect(tiles.map((tile) => tile.getAttribute('href'))).toEqual(
      HELP_PAGES.map((page) => `#/help/${page.slug}`)
    );
    for (const [index, page] of HELP_PAGES.entries()) {
      expect(within(tiles[index]).getByText(page.title)).toBeVisible();
      expect(page.about.length).toBeGreaterThan(0);
      expect(tiles[index]).toHaveTextContent(page.about);
    }
    expect(screen.queryByTestId('no-help-page')).toBeNull();
  });

  it('starts with getting started and the words, then follows the nav', () => {
    expect(HELP_PAGES.map((page) => page.slug)).toEqual([
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
    ]);
  });

  it('gives every page a title of its own, not its file name', () => {
    const titles = HELP_PAGES.map((page) => page.title);
    expect(new Set(titles).size).toBe(titles.length);
    for (const page of HELP_PAGES) {
      expect(page.markdown).toContain(`# ${page.title}`);
    }
  });

  it('says so for a name no page has, and still shows the index', () => {
    render(HelpPage, { slug: 'no-such-page' });
    expect(screen.getByTestId('no-help-page')).toHaveTextContent(
      'There is no help page by that name.'
    );
    expect(screen.getByRole('heading', { level: 1, name: 'Help' })).toBeVisible();
    expect(
      within(screen.getByRole('list', { name: 'Help pages' })).getAllByRole('link')
    ).toHaveLength(HELP_PAGES.length);
    // The name typed into the URL is not echoed back.
    expect(document.body.textContent).not.toContain('no-such-page');
  });
});

describe('one help page', () => {
  it('is titled with the page title and renders its Markdown, with a way back to the index', () => {
    const page = helpPage('run')!;
    render(HelpPage, { slug: 'run' });
    // The frame's title; the Markdown's own first heading may repeat it inside the article.
    expect(screen.getAllByRole('heading', { level: 1, name: page.title }).length).toBeGreaterThan(
      0
    );
    expect(screen.getByRole('link', { name: 'All help pages' })).toHaveAttribute('href', '#/help');
    const article = document.querySelector('article.doc') as HTMLElement;
    expect(article).not.toBeNull();
    // Rendered, not shown as source: no Markdown marks are left over.
    expect(article.textContent).not.toMatch(/^#{1,6} /m);
    expect(article.textContent).not.toContain('](');
    expect(article.textContent).not.toContain('**');
    expect(article.querySelectorAll('p, li').length).toBeGreaterThan(3);
    // A help page has no `?` of its own.
    expect(screen.queryByRole('link', { name: /^Help for/ })).toBeNull();
  });

  it.each(HELP_PAGES.map((page) => [page.slug, page] as const))(
    'renders %s with no Markdown source left showing and no dead link',
    (slug, page) => {
      render(HelpPage, { slug });
      const article = document.querySelector('article.doc') as HTMLElement;
      expect(article.textContent).not.toContain('](');
      expect(article.textContent).not.toMatch(/^#{1,6} /m);
      const slugs = new Set(HELP_PAGES.map((other) => other.slug));
      for (const link of article.querySelectorAll('a')) {
        const href = link.getAttribute('href') ?? '';
        const help = /^#\/help\/(.+)$/.exec(href);
        if (help) {
          expect(slugs, `${page.slug}.md links to ${href}`).toContain(help[1]);
        } else {
          // Anything else is a console page or the help index: never an empty or relative link.
          expect(href, `a link in ${page.slug}.md`).toMatch(/^(#\/|https?:\/\/)/);
        }
      }
    }
  );
});

describe('the links between the console and its help', () => {
  it('finds the screens that have a ?', () => {
    // Guards the two tests below: a glob that matched nothing would let them pass empty.
    const withHelp = Object.values(SCREENS).filter((source) => /\bhelp="[^"]+"/.test(source));
    expect(withHelp.length).toBeGreaterThanOrEqual(10);
  });

  it('every ? beside a page title opens a help page that exists', () => {
    const slugs = new Set(HELP_PAGES.map((page) => page.slug));
    for (const [path, source] of Object.entries(SCREENS)) {
      for (const match of source.matchAll(/\bhelp="([^"]+)"/g)) {
        expect(slugs, `${path} opens help "${match[1]}"`).toContain(match[1]);
      }
      // A help slug worked out at run time could not be checked here.
      expect(source, `${path} passes help as an expression`).not.toMatch(/<Page[^>]*\bhelp=\{/);
    }
  });

  it('every #/help link written in the Markdown points at a page that exists', () => {
    const slugs = new Set(HELP_PAGES.map((page) => page.slug));
    let checked = 0;
    for (const page of HELP_PAGES) {
      for (const match of page.markdown.matchAll(/\(#\/help\/([^)\s]*)\)/g)) {
        checked += 1;
        expect(slugs, `${page.slug}.md links to #/help/${match[1]}`).toContain(match[1]);
      }
    }
    // The pages do link to each other; none found would mean the pattern is wrong.
    expect(checked).toBeGreaterThan(0);
  });

  it('every page of the console that has help is reached from some ?', () => {
    const opened = new Set(
      Object.values(SCREENS).flatMap((source) =>
        [...source.matchAll(/\bhelp="([^"]+)"/g)].map((match) => match[1])
      )
    );
    // Two pages explain an idea, not a console page, and are reached from the index.
    const ideas = ['start-here', 'words'];
    for (const page of HELP_PAGES) {
      if (ideas.includes(page.slug)) continue;
      expect(opened, `nothing opens the help page "${page.slug}"`).toContain(page.slug);
    }
  });
});
