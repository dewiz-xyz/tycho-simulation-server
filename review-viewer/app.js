const contentEl = document.getElementById('content');
const tocEl = document.getElementById('toc');
const themeToggleButton = document.getElementById('theme-toggle');

const markdownPath = '../review_guide.md';
const THEME_KEY = 'review-guide-theme';
const DEFAULT_THEME = 'dark';

const escapeHtml = (value) =>
  value
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');

const inlineFormat = (input) => {
  const escaped = escapeHtml(input);
  return escaped
    .replace(/`([^`]+)`/g, '<code>$1</code>')
    .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
    .replace(/\[([^\]]+)\]\(([^)]+)\)/g, '<a href="$2" target="_blank" rel="noreferrer">$1</a>');
};

const trimPipes = (value) => value.replace(/^\|/, '').replace(/\|$/, '');

const slugify = (() => {
  const seen = new Map();
  return (text) => {
    const base = text
      .toLowerCase()
      .replace(/[^a-z0-9\s-]/g, '')
      .trim()
      .replace(/\s+/g, '-');

    const count = seen.get(base) ?? 0;
    seen.set(base, count + 1);
    return count === 0 ? base : `${base}-${count}`;
  };
})();

const parseTable = (lines, startAt) => {
  const headerLine = lines[startAt];
  const dividerLine = lines[startAt + 1] ?? '';

  if (!headerLine?.includes('|') || !/^\s*\|?\s*[-:| ]+\s*$/.test(dividerLine)) {
    return null;
  }

  const headerCells = trimPipes(headerLine)
    .split('|')
    .map((cell) => inlineFormat(cell.trim()));

  const rows = [];
  let index = startAt + 2;

  while (index < lines.length && lines[index].includes('|')) {
    const raw = trimPipes(lines[index]);
    const cells = raw.split('|').map((cell) => inlineFormat(cell.trim()));
    rows.push(cells);
    index += 1;
  }

  const tableHtml = [
    '<table>',
    '<thead>',
    '<tr>',
    ...headerCells.map((cell) => `<th>${cell}</th>`),
    '</tr>',
    '</thead>',
    '<tbody>',
    ...rows.map((row) => `<tr>${row.map((cell) => `<td>${cell}</td>`).join('')}</tr>`),
    '</tbody>',
    '</table>',
  ].join('');

  return { html: tableHtml, nextIndex: index };
};

const metadataMatch = (line) => {
  const match = line.match(/^(Change summary|Risk level|Review focus|Hunk context notes):\s*(.+)$/i);
  if (!match) return null;
  return {
    label: match[1],
    value: match[2],
  };
};

const parseMarkdown = (markdown) => {
  const lines = markdown.replace(/\r\n/g, '\n').split('\n');
  const htmlParts = [];

  let i = 0;
  let paragraph = [];

  const flushParagraph = () => {
    if (!paragraph.length) return;
    const joined = paragraph.map((line) => inlineFormat(line)).join('<br />');
    htmlParts.push(`<p>${joined}</p>`);
    paragraph = [];
  };

  while (i < lines.length) {
    const line = lines[i] ?? '';

    if (line.startsWith('```')) {
      flushParagraph();
      const lang = line.slice(3).trim().toLowerCase();
      i += 1;
      const chunk = [];
      while (i < lines.length && !lines[i].startsWith('```')) {
        chunk.push(lines[i]);
        i += 1;
      }
      htmlParts.push(
        `<pre class="${lang === 'diff' ? 'diff' : ''}"><code data-lang="${escapeHtml(lang)}">${escapeHtml(chunk.join('\n'))}</code></pre>`
      );
      i += 1;
      continue;
    }

    const headingMatch = line.match(/^(#{1,6})\s+(.+)$/);
    if (headingMatch) {
      flushParagraph();
      const level = headingMatch[1].length;
      const text = headingMatch[2].trim();
      const id = slugify(text);
      htmlParts.push(`<h${level} id="${id}">${inlineFormat(text)}</h${level}>`);
      i += 1;
      continue;
    }

    const table = parseTable(lines, i);
    if (table) {
      flushParagraph();
      htmlParts.push(table.html);
      i = table.nextIndex;
      continue;
    }

    const metadata = metadataMatch(line.trim());
    if (metadata) {
      flushParagraph();
      htmlParts.push(
        `<p class="meta-line" data-meta-label="${escapeHtml(metadata.label)}"><span class="meta-label">${escapeHtml(metadata.label)}</span><span class="meta-value">${inlineFormat(metadata.value)}</span></p>`
      );
      i += 1;
      continue;
    }

    if (/^\s*[-*]\s+/.test(line)) {
      flushParagraph();
      const items = [];
      while (i < lines.length && /^\s*[-*]\s+/.test(lines[i])) {
        items.push(lines[i].replace(/^\s*[-*]\s+/, '').trim());
        i += 1;
      }
      htmlParts.push(`<ul>${items.map((item) => `<li>${inlineFormat(item)}</li>`).join('')}</ul>`);
      continue;
    }

    if (/^\s*\d+\.\s+/.test(line)) {
      flushParagraph();
      const items = [];
      while (i < lines.length && /^\s*\d+\.\s+/.test(lines[i])) {
        items.push(lines[i].replace(/^\s*\d+\.\s+/, '').trim());
        i += 1;
      }
      htmlParts.push(`<ol>${items.map((item) => `<li>${inlineFormat(item)}</li>`).join('')}</ol>`);
      continue;
    }

    if (line.trim() === '') {
      flushParagraph();
      i += 1;
      continue;
    }

    paragraph.push(line.trim());
    i += 1;
  }

  flushParagraph();
  return htmlParts.join('\n');
};

const isFileHeading = (text) => /^(src|examples)\//.test(text.trim());

// #11 Diff line numbers — enhanced colorize that tracks hunk offsets
const colorizeDiffBlocks = () => {
  const blocks = contentEl.querySelectorAll('pre.diff code');

  blocks.forEach((codeEl) => {
    const lines = codeEl.textContent.split('\n');
    let oldLine = 0;
    let newLine = 0;

    const decorated = lines
      .map((line) => {
        let className = 'line';
        let oldNum = '';
        let newNum = '';

        if (
          line.startsWith('diff --git') ||
          line.startsWith('index ') ||
          line.startsWith('--- ') ||
          line.startsWith('+++ ') ||
          line.startsWith('new file mode') ||
          line.startsWith('deleted file mode')
        ) {
          className += ' diff-meta';
        } else if (line.startsWith('@@')) {
          className += ' diff-hunk';
          // Parse hunk header: @@ -oldStart,oldCount +newStart,newCount @@
          const hunkMatch = line.match(/@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/);
          if (hunkMatch) {
            oldLine = parseInt(hunkMatch[1], 10);
            newLine = parseInt(hunkMatch[2], 10);
          }
        } else if (line.startsWith('+')) {
          className += ' diff-add';
          newNum = newLine;
          newLine++;
        } else if (line.startsWith('-')) {
          className += ' diff-del';
          oldNum = oldLine;
          oldLine++;
        } else {
          // Context line
          if (oldLine > 0) {
            oldNum = oldLine;
            newNum = newLine;
            oldLine++;
            newLine++;
          }
        }

        const oldLn = oldNum !== '' ? `<span class="ln ln-old">${oldNum}</span>` : '<span class="ln ln-old"> </span>';
        const newLn = newNum !== '' ? `<span class="ln ln-new">${newNum}</span>` : '<span class="ln ln-new"> </span>';

        // Only show line numbers for non-meta, non-hunk lines
        const showNums = !className.includes('diff-meta') && !className.includes('diff-hunk');
        const prefix = showNums ? `${oldLn}${newLn}` : '';

        return `<span class="${className}">${prefix}${escapeHtml(line) || ' '}</span>`;
      })
      .join('');

    codeEl.innerHTML = decorated;
  });
};

// #5 Diff +/− counts in panel labels
const enhanceFileCards = () => {
  const headings = [...contentEl.querySelectorAll('h2')];

  headings.forEach((heading) => {
    if (!isFileHeading(heading.textContent)) return;

    const card = document.createElement('section');
    card.className = 'file-card';
    heading.classList.add('file-title');

    heading.parentNode.insertBefore(card, heading);
    card.appendChild(heading);

    let cursor = card.nextSibling;
    while (cursor) {
      const next = cursor.nextSibling;

      if (cursor.nodeType === Node.ELEMENT_NODE) {
        const tag = cursor.tagName;
        if (tag === 'H2' || tag === 'H3') break;
      }

      card.appendChild(cursor);
      cursor = next;
    }

    const metaLines = [...card.querySelectorAll('p.meta-line')];
    const metaGrid = document.createElement('div');
    metaGrid.className = 'meta-grid';

    let riskLevel = null;

    metaLines.forEach((line) => {
      const label = line.dataset.metaLabel ?? 'Meta';
      const valueEl = line.querySelector('.meta-value');
      const valueHtml = valueEl ? valueEl.innerHTML : '';

      const item = document.createElement('article');
      item.className = 'meta-item';
      item.innerHTML = `<p class="meta-item-label">${escapeHtml(label)}</p><p class="meta-item-value">${valueHtml}</p>`;
      metaGrid.appendChild(item);

      if (label.toLowerCase() === 'risk level') {
        riskLevel = valueEl?.textContent.trim().toLowerCase() ?? null;
      }

      line.remove();
    });

    if (metaGrid.children.length > 0) {
      const firstDiff = card.querySelector('pre');
      if (firstDiff) {
        card.insertBefore(metaGrid, firstDiff);
      } else {
        card.appendChild(metaGrid);
      }
    }

    if (riskLevel) {
      card.dataset.risk = riskLevel;
    }

    const headingWrap = document.createElement('div');
    headingWrap.className = 'file-heading';
    heading.parentNode.insertBefore(headingWrap, heading);
    headingWrap.appendChild(heading);

    if (riskLevel) {
      const badge = document.createElement('span');
      badge.className = `risk-pill risk-${riskLevel}`;
      badge.textContent = `${riskLevel} risk`;
      headingWrap.appendChild(badge);
    }

    // Wrap diff blocks in details panels with +/− stats
    const diffBlocks = [...card.querySelectorAll('pre.diff')];
    diffBlocks.forEach((diffBlock) => {
      const addCount = diffBlock.querySelectorAll('.line.diff-add').length;
      const delCount = diffBlock.querySelectorAll('.line.diff-del').length;
      const lineCount = diffBlock.querySelectorAll('.line').length;

      const details = document.createElement('details');
      details.className = 'diff-panel';

      const summary = document.createElement('summary');
      const statsHtml = `<span class="diff-stat-add">+${addCount}</span> <span class="diff-stat-del">−${delCount}</span> · ${lineCount.toLocaleString()} lines`;
      summary.innerHTML = `<span>${statsHtml}</span><span class="diff-count">View diff</span>`;

      diffBlock.replaceWith(details);
      details.append(summary, diffBlock);
    });
  });
};

// #9 Phase header badges
const enhancePhaseHeaders = () => {
  const headers = [...contentEl.querySelectorAll('h3')];

  headers.forEach((h3) => {
    // Count file cards and high-risk items between this h3 and the next one
    let fileCount = 0;
    let highCount = 0;

    let cursor = h3.nextElementSibling;
    while (cursor) {
      if (cursor.tagName === 'H3') break;
      if (cursor.classList.contains('file-card')) {
        fileCount++;
        if (cursor.dataset.risk === 'high') highCount++;
      }
      cursor = cursor.nextElementSibling;
    }

    if (fileCount > 0) {
      const meta = document.createElement('span');
      meta.className = 'phase-meta';
      let text = `${fileCount} file${fileCount !== 1 ? 's' : ''}`;
      if (highCount > 0) text += ` · ${highCount} high`;
      meta.textContent = text;
      h3.appendChild(meta);
    }
  });
};

// #3 Risk dots in ToC + #12 Phase count in ToC
const buildToc = () => {
  const sections = [...contentEl.children];
  const groups = [];
  let currentGroup = null;

  sections.forEach((section) => {
    if (section.tagName === 'H3') {
      currentGroup = {
        id: section.id,
        title: section.textContent.trim(),
        files: [],
      };
      groups.push(currentGroup);
      return;
    }

    if (section.classList.contains('file-card')) {
      const heading = section.querySelector('.file-title');
      if (!heading) return;

      if (!currentGroup) {
        currentGroup = {
          id: heading.id,
          title: 'Sections',
          files: [],
        };
        groups.push(currentGroup);
      }

      currentGroup.files.push({
        id: heading.id,
        title: heading.textContent.trim(),
        risk: section.dataset.risk || null,
      });
    }
  });

  if (!groups.length) {
    tocEl.innerHTML = '<p>No sections found.</p>';
    return;
  }

  tocEl.innerHTML = groups
    .map((group) => {
      // Strip the phase-meta text from the display title (it was appended to the h3)
      const cleanTitle = group.title.replace(/\d+ files?(\s+·\s+\d+ high)?$/, '').trim();

      const highCount = group.files.filter((f) => f.risk === 'high').length;
      let phaseCountHtml = '';
      if (group.files.length > 0) {
        let countText = `${group.files.length} file${group.files.length !== 1 ? 's' : ''}`;
        if (highCount > 0) countText += ` · ${highCount} high`;
        phaseCountHtml = `<span class="toc-phase-count">${escapeHtml(countText)}</span>`;
      }

      const files = group.files
        .map((file) => {
          const dotHtml = file.risk
            ? `<span class="toc-risk-dot toc-risk-${file.risk}"></span>`
            : '';
          return `<a class="toc-file" href="#${file.id}" data-target="${file.id}">${dotHtml}<span>${escapeHtml(file.title)}</span></a>`;
        })
        .join('');
      return `<section class="toc-group"><a class="toc-phase" href="#${group.id}" data-target="${group.id}">${escapeHtml(cleanTitle)}</a>${phaseCountHtml}${files}</section>`;
    })
    .join('');
};

// #4 Dynamic header with stats
const enhanceHeader = () => {
  const h1 = contentEl.querySelector('h1');
  if (!h1) return;

  // Try to extract branch range from the H1 text (e.g., "Review Guide: master...HEAD")
  const branchMatch = h1.textContent.match(/:\s*(.+)/);
  const brandH1 = document.querySelector('.topbar .brand h1');
  if (branchMatch && brandH1) {
    brandH1.textContent = `Review Guide`;
  }

  // Extract file/insertion/deletion counts from the Diff Snapshot list
  const allCards = contentEl.querySelectorAll('.file-card');
  const fileCount = allCards.length;

  // Count insertions/deletions from all diff panels
  let totalAdd = 0;
  let totalDel = 0;
  contentEl.querySelectorAll('.line.diff-add').forEach(() => totalAdd++);
  contentEl.querySelectorAll('.line.diff-del').forEach(() => totalDel++);

  // Count risk distribution
  const riskCounts = { high: 0, medium: 0, low: 0 };
  allCards.forEach((card) => {
    const risk = card.dataset.risk;
    if (risk && riskCounts[risk] !== undefined) riskCounts[risk]++;
  });

  // Update subtitle
  const subtitle = document.querySelector('.topbar .subtitle');
  if (subtitle) {
    let subtitleText = `${fileCount} files changed`;
    if (totalAdd > 0 || totalDel > 0) {
      subtitleText += ` · +${totalAdd.toLocaleString()} −${totalDel.toLocaleString()}`;
    }
    if (branchMatch) {
      subtitleText += ` · ${branchMatch[1].trim()}`;
    }
    subtitle.textContent = subtitleText;
  }

  // Add stats bar with risk chips
  const brand = document.querySelector('.topbar .brand');
  if (brand && (riskCounts.high > 0 || riskCounts.medium > 0 || riskCounts.low > 0)) {
    const statsBar = document.createElement('div');
    statsBar.className = 'stats-bar';

    const chips = [];
    if (riskCounts.high > 0) {
      chips.push(`<span class="stats-chip stats-chip-high">${riskCounts.high} high</span>`);
    }
    if (riskCounts.medium > 0) {
      chips.push(`<span class="stats-chip stats-chip-medium">${riskCounts.medium} medium</span>`);
    }
    if (riskCounts.low > 0) {
      chips.push(`<span class="stats-chip stats-chip-low">${riskCounts.low} low</span>`);
    }
    statsBar.innerHTML = chips.join('');
    brand.appendChild(statsBar);
  }
};

// #6 Active section highlighting via IntersectionObserver
const setupScrollSpy = () => {
  const tocLinks = [...tocEl.querySelectorAll('.toc-file, .toc-phase')];
  if (!tocLinks.length) return;

  const targetMap = new Map();
  tocLinks.forEach((link) => {
    const targetId = link.getAttribute('data-target');
    if (targetId) targetMap.set(targetId, link);
  });

  // Observe h3 phase headers and file-card elements
  const observeTargets = [
    ...contentEl.querySelectorAll('h3[id]'),
    ...contentEl.querySelectorAll('.file-card .file-title[id]'),
  ];

  if (!observeTargets.length) return;

  const observer = new IntersectionObserver(
    (entries) => {
      entries.forEach((entry) => {
        if (entry.isIntersecting) {
          tocLinks.forEach((link) => link.classList.remove('toc-active'));

          const targetId = entry.target.id;
          const activeLink = targetMap.get(targetId);
          if (activeLink) {
            activeLink.classList.add('toc-active');
            // Also highlight the parent phase if a file is active
            if (activeLink.classList.contains('toc-file')) {
              const group = activeLink.closest('.toc-group');
              const phaseLink = group?.querySelector('.toc-phase');
              if (phaseLink) phaseLink.classList.add('toc-active');
            }
          }
        }
      });
    },
    { rootMargin: '-80px 0px -60% 0px', threshold: 0 }
  );

  observeTargets.forEach((el) => observer.observe(el));
};

// #7 Expand/collapse all diffs
const setupExpandCollapseAll = () => {
  const actionsEl = document.querySelector('.topbar-actions');
  if (!actionsEl) return;

  const btn = document.createElement('button');
  btn.type = 'button';
  btn.className = 'button button-ghost';
  btn.textContent = 'Expand all';
  btn.setAttribute('aria-label', 'Toggle all diff panels');

  let allOpen = false;

  btn.addEventListener('click', () => {
    const panels = contentEl.querySelectorAll('details.diff-panel');
    allOpen = !allOpen;
    panels.forEach((panel) => {
      panel.open = allOpen;
    });
    btn.textContent = allOpen ? 'Collapse all' : 'Expand all';
  });

  // Insert before the first button
  actionsEl.insertBefore(btn, actionsEl.firstChild);
};

// #10 Keyboard navigation
const setupKeyboardNav = () => {
  const getCards = () => [...contentEl.querySelectorAll('.file-card')];
  let focusIndex = -1;

  const focusCard = (index) => {
    const cards = getCards();
    if (index < 0 || index >= cards.length) return;

    // Remove previous focus
    cards.forEach((c) => c.classList.remove('kbd-focus'));
    focusIndex = index;
    cards[focusIndex].classList.add('kbd-focus');
    cards[focusIndex].scrollIntoView({ behavior: 'smooth', block: 'center' });
  };

  document.addEventListener('keydown', (e) => {
    // Don't capture if user is typing in an input/textarea
    if (e.target.tagName === 'INPUT' || e.target.tagName === 'TEXTAREA') return;

    const cards = getCards();
    if (!cards.length) return;

    switch (e.key.toLowerCase()) {
      case 'j': // Next file card
        e.preventDefault();
        focusCard(Math.min(focusIndex + 1, cards.length - 1));
        break;
      case 'k': // Previous file card
        e.preventDefault();
        focusCard(Math.max(focusIndex - 1, 0));
        break;
      case 'h': { // Jump to next high-risk file
        e.preventDefault();
        const highCards = cards
          .map((c, i) => ({ card: c, index: i }))
          .filter(({ card }) => card.dataset.risk === 'high');
        if (!highCards.length) break;
        // Find next high-risk card after current focus
        const next = highCards.find(({ index }) => index > focusIndex) || highCards[0];
        focusCard(next.index);
        break;
      }
      case 'e': { // Toggle diff panels in current card
        e.preventDefault();
        if (focusIndex < 0 || focusIndex >= cards.length) break;
        const panels = cards[focusIndex].querySelectorAll('details.diff-panel');
        const anyOpen = [...panels].some((p) => p.open);
        panels.forEach((p) => { p.open = !anyOpen; });
        break;
      }
    }
  });

  // Add keyboard hints to topbar
  const actionsEl = document.querySelector('.topbar-actions');
  if (actionsEl) {
    const hint = document.createElement('div');
    hint.className = 'kbd-hint';
    hint.innerHTML = '<kbd>J</kbd><kbd>K</kbd> nav <kbd>H</kbd> high <kbd>E</kbd> diff';
    actionsEl.appendChild(hint);
  }
};

const applyTheme = (theme) => {
  document.documentElement.setAttribute('data-theme', theme);
  themeToggleButton.textContent = theme === 'dark' ? 'Light mode' : 'Dark mode';
  themeToggleButton.setAttribute('aria-label', theme === 'dark' ? 'Switch to light mode' : 'Switch to dark mode');
};

const setupThemeToggle = () => {
  const savedTheme = localStorage.getItem(THEME_KEY) || DEFAULT_THEME;
  applyTheme(savedTheme);

  themeToggleButton.addEventListener('click', () => {
    const currentTheme = document.documentElement.getAttribute('data-theme') || DEFAULT_THEME;
    const nextTheme = currentTheme === 'dark' ? 'light' : 'dark';

    localStorage.setItem(THEME_KEY, nextTheme);
    applyTheme(nextTheme);
  });
};

const render = async () => {
  try {
    const response = await fetch(markdownPath, { cache: 'no-store' });
    if (!response.ok) {
      throw new Error(`Failed to load markdown (${response.status})`);
    }

    const markdown = await response.text();
    contentEl.innerHTML = parseMarkdown(markdown);

    colorizeDiffBlocks();     // needs raw pre.diff blocks
    enhanceFileCards();        // wraps cards, extracts risk, creates diff panels
    enhancePhaseHeaders();     // needs file-cards with data-risk
    buildToc();                // needs file-cards and phase headers
    enhanceHeader();           // needs file-cards for risk counts
    setupScrollSpy();          // needs ToC links
    setupExpandCollapseAll();  // needs diff panels
    setupKeyboardNav();        // needs file-cards
  } catch (error) {
    contentEl.innerHTML = `<p>Could not load review guide: ${escapeHtml(String(error))}</p>`;
  }
};

setupThemeToggle();
void render();
