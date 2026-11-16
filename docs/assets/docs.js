/* Keep overflowed examples reachable by keyboard, including after a resize. */
(() => {
  const content = document.querySelector('.doc-content');
  if (!content) return;
  const scrollers = [...content.querySelectorAll('pre, table')];
  const updateScrollers = () => {
    for (const element of scrollers) {
      if (element.scrollWidth > element.clientWidth + 1) {
        element.tabIndex = 0;
        if (element.tagName === 'PRE') {
          element.setAttribute('role', 'region');
          element.setAttribute('aria-label', 'Code example (scroll horizontally)');
        }
      } else {
        element.removeAttribute('tabindex');
        if (element.tagName === 'PRE') {
          element.removeAttribute('role');
          element.removeAttribute('aria-label');
        }
      }
    }
  };
  updateScrollers();
  window.addEventListener('resize', updateScrollers);
  document.fonts?.ready.then(updateScrollers);
  for (const checkbox of content.querySelectorAll('input[type="checkbox"][disabled]')) {
    const text = checkbox.closest('li')?.textContent.trim();
    if (text) checkbox.setAttribute('aria-label', text);
  }
  for (const link of document.querySelectorAll('.doc-sidebar nav a')) {
    if (new URL(link.href).pathname === location.pathname) link.setAttribute('aria-current', 'page');
  }
})();
