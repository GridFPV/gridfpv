/** The small Markdown renderer behind the help pages (`lib/markdown.ts`). */
import { describe, expect, it } from 'vitest';
import { firstParagraph, inline, renderMarkdown, titleOf } from '../src/lib/markdown.js';

describe('inline', () => {
  it('leaves plain text as it is', () => {
    expect(inline('Press Start to race.')).toBe('Press Start to race.');
    expect(inline('')).toBe('');
  });

  it('escapes HTML, so a page is data and never markup', () => {
    expect(inline('a < b & "c" > d')).toBe('a &lt; b &amp; &quot;c&quot; &gt; d');
    expect(inline('<script>alert(1)</script>')).toBe('&lt;script&gt;alert(1)&lt;/script&gt;');
    expect(inline('<b>bold</b>')).not.toContain('<b>');
  });

  it('marks inline code, bold and italic', () => {
    expect(inline('Press `Space` now')).toBe('Press <code>Space</code> now');
    expect(inline('a **bold** word')).toBe('a <strong>bold</strong> word');
    expect(inline('an *italic* word')).toBe('an <em>italic</em> word');
    expect(inline('*italic* first')).toBe('<em>italic</em> first');
  });

  it('marks bold and italic in the same line without mixing them up', () => {
    expect(inline('**Run**: put a *heat* on the timer')).toBe(
      '<strong>Run</strong>: put a <em>heat</em> on the timer'
    );
  });

  it('marks more than one of a kind in a line', () => {
    expect(inline('`a` and `b`, **c** and **d**')).toBe(
      '<code>a</code> and <code>b</code>, <strong>c</strong> and <strong>d</strong>'
    );
  });

  it('escapes HTML inside code and bold too', () => {
    expect(inline('`<dialog>` and **<b>**')).toBe(
      '<code>&lt;dialog&gt;</code> and <strong>&lt;b&gt;</strong>'
    );
  });

  it('leaves a lone mark that opens nothing as it is', () => {
    expect(inline('5 * 3')).toBe('5 * 3');
    expect(inline('a ` tick')).toBe('a ` tick');
    expect(inline('**not closed')).toBe('**not closed');
  });

  it('links to a console page in the same tab', () => {
    expect(inline('See [Run](#/help/run).')).toBe('See <a href="#/help/run">Run</a>.');
  });

  it('opens a link out to the web in a new tab, without handing over the opener', () => {
    expect(inline('[the site](https://example.com/a?b=1)')).toBe(
      '<a href="https://example.com/a?b=1" rel="noopener" target="_blank">the site</a>'
    );
    expect(inline('[plain](http://example.com)')).toBe(
      '<a href="http://example.com" rel="noopener" target="_blank">plain</a>'
    );
  });

  it('sends a link to anything else to the help index: it goes nowhere', () => {
    expect(inline('[x](javascript:void)')).toBe('<a href="#/help">x</a>');
    expect(inline('[x](data:text/html,hi)')).toBe('<a href="#/help">x</a>');
    expect(inline('[x](run.md)')).toBe('<a href="#/help">x</a>');
    expect(inline('[x](/pilots)')).toBe('<a href="#/help">x</a>');
    expect(inline('[x](#top)')).toBe('<a href="#/help">x</a>');
  });

  it('cannot be made to break out of the link with a quote in the address', () => {
    expect(inline('[x](https://e.com/"onmouseover="alert(1))')).not.toMatch(/"\s*onmouseover=/);
    expect(inline('[x](#/help/"><script>)')).not.toContain('<script>');
  });

  it('marks the words of a link', () => {
    expect(inline('[**Start** here](#/help/start-here)')).toBe(
      '<a href="#/help/start-here"><strong>Start</strong> here</a>'
    );
  });

  it('leaves square brackets with no address as they are', () => {
    expect(inline('[not a link]')).toBe('[not a link]');
    expect(inline('[a](b c)')).toBe('[a](b c)');
  });
});

describe('renderMarkdown', () => {
  it('renders nothing for an empty page', () => {
    expect(renderMarkdown('')).toBe('');
    expect(renderMarkdown('\n\n  \n')).toBe('');
  });

  it('renders headings of three levels', () => {
    expect(renderMarkdown('# One\n## Two\n### Three')).toBe(
      '<h1>One</h1>\n<h2>Two</h2>\n<h3>Three</h3>'
    );
  });

  it('takes a deeper heading, or a # with no space, as words', () => {
    expect(renderMarkdown('#### Four')).toBe('<p>#### Four</p>');
    expect(renderMarkdown('#hashtag')).toBe('<p>#hashtag</p>');
  });

  it('joins the lines of a paragraph, and parts paragraphs at a blank line', () => {
    expect(renderMarkdown('First line\nsecond line.\n\nNext paragraph.')).toBe(
      '<p>First line second line.</p>\n<p>Next paragraph.</p>'
    );
  });

  it('renders a bulleted list, with - or *', () => {
    expect(renderMarkdown('- a\n- b')).toBe('<ul><li>a</li><li>b</li></ul>');
    expect(renderMarkdown('* a\n* b')).toBe('<ul><li>a</li><li>b</li></ul>');
  });

  it('renders a numbered list', () => {
    expect(renderMarkdown('1. first\n2. second\n10. tenth')).toBe(
      '<ol><li>first</li><li>second</li><li>tenth</li></ol>'
    );
  });

  it('carries an indented line on into the list item above it', () => {
    expect(
      renderMarkdown('1. **Seats**: set the channels. See\n   [Seats](#/help/seats).\n2. Next')
    ).toBe(
      '<ol><li><strong>Seats</strong>: set the channels. See <a href="#/help/seats">Seats</a>.</li><li>Next</li></ol>'
    );
  });

  it('starts a new list when the kind changes, and when a blank line parts two lists', () => {
    expect(renderMarkdown('- a\n1. b')).toBe('<ul><li>a</li></ul>\n<ol><li>b</li></ol>');
    expect(renderMarkdown('- a\n\n- b')).toBe('<ul><li>a</li></ul>\n<ul><li>b</li></ul>');
  });

  it('ends a paragraph at a heading or a list with no blank line between', () => {
    expect(renderMarkdown('Words\n## Heading\nMore\n- item\nAfter')).toBe(
      '<p>Words</p>\n<h2>Heading</h2>\n<p>More</p>\n<ul><li>item</li></ul>\n<p>After</p>'
    );
  });

  it('does not take a line that starts with bold or italic for a list item', () => {
    expect(renderMarkdown('**Overlays** gives you pages.')).toBe(
      '<p><strong>Overlays</strong> gives you pages.</p>'
    );
    expect(renderMarkdown('*Note* this.')).toBe('<p><em>Note</em> this.</p>');
  });

  it('marks and escapes the words of headings, paragraphs and list items alike', () => {
    expect(renderMarkdown('# A `b` <c>\n\nd **e** <f>\n\n- g *h* <i>')).toBe(
      '<h1>A <code>b</code> &lt;c&gt;</h1>\n' +
        '<p>d <strong>e</strong> &lt;f&gt;</p>\n' +
        '<ul><li>g <em>h</em> &lt;i&gt;</li></ul>'
    );
  });

  it('reads Windows and old Mac line endings, and drops spaces at the end of a line', () => {
    expect(renderMarkdown('# Title  \r\n\r\nOne\r\ntwo\r\n\r\n- a\r- b\r')).toBe(
      '<h1>Title</h1>\n<p>One two</p>\n<ul><li>a</li><li>b</li></ul>'
    );
  });

  it('renders a whole page in order', () => {
    const page = [
      '# Start here',
      '',
      'Grid runs a race day.',
      '',
      '## In order',
      '',
      '1. **Event**: name the day.',
      '2. **Pilots**: add who is flying.',
      '',
      'See [Words](#/help/words).'
    ].join('\n');
    expect(renderMarkdown(page).split('\n')).toEqual([
      '<h1>Start here</h1>',
      '<p>Grid runs a race day.</p>',
      '<h2>In order</h2>',
      '<ol><li><strong>Event</strong>: name the day.</li><li><strong>Pilots</strong>: add who is flying.</li></ol>',
      '<p>See <a href="#/help/words">Words</a>.</p>'
    ]);
  });
});

describe('titleOf', () => {
  it('is the first top-level heading of the page', () => {
    expect(titleOf('# Start here\n\nWords.')).toBe('Start here');
    expect(titleOf('Words first.\n\n#   Late title  \n\n# Second')).toBe('Late title');
  });

  it('is the fallback for a page with no top-level heading', () => {
    expect(titleOf('## Only a section\n\nWords.')).toBe('Help');
    expect(titleOf('')).toBe('Help');
    expect(titleOf('#nospace', 'run')).toBe('run');
  });
});

describe('firstParagraph', () => {
  it('is the first paragraph after the title, on one line', () => {
    expect(firstParagraph('# Run\n\nPut a heat on the timer,\nthen start.\n\nMore.')).toBe(
      'Put a heat on the timer, then start.'
    );
  });

  it('is plain text: marks are dropped and a link keeps only its words', () => {
    expect(
      firstParagraph('# Run\n\nPress **Start** or `Space`. See [Words *Grid* uses](#/help/words).')
    ).toBe('Press Start or Space. See Words Grid uses.');
  });

  it('skips a section heading that comes before the first paragraph', () => {
    expect(firstParagraph('# Run\n\n## Before you start\n\nPick a heat.')).toBe('Pick a heat.');
  });

  it('works on a page with no title, and with Windows line endings', () => {
    expect(firstParagraph('Just words.\n\nMore.')).toBe('Just words.');
    expect(firstParagraph('# Run\r\n\r\nPick a heat.\r\n\r\nMore.')).toBe('Pick a heat.');
  });

  it('is empty for a page with no paragraph', () => {
    expect(firstParagraph('')).toBe('');
    expect(firstParagraph('# Only a title')).toBe('');
    expect(firstParagraph('# Title\n\n## Section')).toBe('');
  });
});
