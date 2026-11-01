// Added to every artifact tamale serves (see ARTIFACT_FIT in src/web.rs). On a narrow
// screen nothing should need sideways scrolling, whatever the page was written to do:
// code wraps, tables wider than their box become stacked cards, boxes that scroll sideways
// wrap their contents, and anything wider than the page is held to its width.
(() => {
  const NARROW = 640;
  const css = document.createElement('style');
  css.textContent = `@media (max-width: ${NARROW}px) {
  pre, pre *, code, kbd, samp { white-space: pre-wrap !important; overflow-wrap: anywhere !important; }
  img, video, canvas, iframe, svg[viewBox] { max-width: 100% !important; height: auto; }
  table.tamale-stack, table.tamale-stack tbody, table.tamale-stack tfoot,
  table.tamale-stack tr, table.tamale-stack td, table.tamale-stack th { display: block !important; width: auto !important; min-width: 0 !important; }
  table.tamale-stack thead { display: none !important; }
  table.tamale-stack tr { border: 1px solid rgba(128, 128, 128, 0.28); border-radius: 10px; padding: 8px 12px; margin: 0 0 10px; }
  table.tamale-stack td, table.tamale-stack th { padding: 4px 0 !important; border: none !important; text-align: left !important; white-space: normal !important; overflow-wrap: anywhere; }
  table.tamale-stack [data-label]:not([data-label=""])::before { content: attr(data-label); display: block; font-size: 0.78em; opacity: 0.62; letter-spacing: 0.02em; margin-bottom: 1px; }
  .tamale-wrap { flex-wrap: wrap !important; overflow-x: visible !important; }
  .tamale-wrap, .tamale-wrap * { white-space: pre-wrap !important; overflow-wrap: anywhere !important; }
  .tamale-fit { max-width: 100% !important; min-width: 0 !important; box-sizing: border-box !important; }
  .tamale-onecol { grid-template-columns: minmax(0, 1fr) !important; }
  .tamale-flexwrap { flex-wrap: wrap !important; }
}`;
  document.head ? document.head.append(css) : document.documentElement.append(css);

  // Not innerWidth: on a phone, content that overflows widens the page's viewport with it.
  const narrow = matchMedia(`(max-width: ${NARROW}px)`);
  const scrolls = (el) => /auto|scroll/.test(getComputedStyle(el).overflowX);
  const elements = () => [...document.body.querySelectorAll('*')].filter((el) => !el.ownerSVGElement);

  function stackTables(width) {
    for (const table of document.querySelectorAll('table:not(.tamale-stack)')) {
      const room = Math.min(width, table.parentElement?.clientWidth || width);
      if (table.scrollWidth <= room + 1) continue;
      const head = table.tHead?.rows[0] ?? (table.rows[0] && [...table.rows[0].cells].every((c) => c.tagName === 'TH') ? table.rows[0] : null);
      const labels = head ? [...head.cells].map((c) => c.textContent.trim()) : [];
      for (const row of table.rows) {
        if (row === head) {
          if (!table.tHead) row.style.display = 'none';
          continue;
        }
        let col = 0;
        for (const cell of row.cells) {
          cell.dataset.label = labels[col] ?? '';
          col += cell.colSpan || 1;
        }
      }
      table.classList.add('tamale-stack');
    }
  }

  function fit() {
    if (!document.body || !narrow.matches) return;
    const width = Math.min(document.documentElement.clientWidth, screen.width || Infinity);
    stackTables(width);
    // Boxes meant to scroll sideways (code, tab rows, wide strips) wrap instead.
    for (const el of elements()) {
      if (el.scrollWidth > el.clientWidth + 1 && scrolls(el)) el.classList.add('tamale-wrap');
    }
    // Whatever still sticks out past the page is held to its width.
    for (const el of elements()) {
      const box = el.getBoundingClientRect();
      if (box.width <= width + 1 && box.right <= width + 1) continue;
      el.classList.add('tamale-fit');
      const style = getComputedStyle(el);
      if (style.display.includes('grid')) el.classList.add('tamale-onecol');
      if (style.display.includes('flex')) el.classList.add('tamale-flexwrap');
      // A grid or flex row can stick out through its children while its own box fits.
      const parent = el.parentElement;
      const outer = parent ? getComputedStyle(parent).display : '';
      if (outer.includes('grid')) parent.classList.add('tamale-onecol');
      if (outer.includes('flex')) parent.classList.add('tamale-flexwrap');
    }
  }

  let queued = false;
  const later = () => {
    if (queued) return;
    queued = true;
    requestAnimationFrame(() => {
      queued = false;
      fit();
    });
  };
  addEventListener('DOMContentLoaded', later);
  addEventListener('load', () => {
    later();
    // Charts and other scripts often draw after load.
    setTimeout(later, 300);
    setTimeout(later, 1200);
  });
  addEventListener('resize', later);
  narrow.addEventListener('change', later);
  later();
})();
