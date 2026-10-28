// `use:tooltip={{ text, side }}`: a small label shown after a short hover delay.

interface Options {
  text: string;
  side?: 'right' | 'bottom' | 'top';
  /** false disables it (e.g. labels already visible). */
  enabled?: boolean;
}

export function tooltip(node: HTMLElement, options: Options) {
  let opts = options;
  let tip: HTMLDivElement | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;

  function show() {
    if (opts.enabled === false || tip) return;
    tip = document.createElement('div');
    tip.className = 'tooltip';
    tip.textContent = opts.text;
    tip.setAttribute('role', 'tooltip');
    document.body.append(tip);

    const r = node.getBoundingClientRect();
    const t = tip.getBoundingClientRect();
    const side = opts.side ?? 'right';
    const gap = 8;
    let x = r.right + gap;
    let y = r.top + r.height / 2 - t.height / 2;
    if (side === 'bottom' || side === 'top') {
      x = r.left + r.width / 2 - t.width / 2;
      y = side === 'bottom' ? r.bottom + gap : r.top - t.height - gap;
    }
    x = Math.min(Math.max(6, x), innerWidth - t.width - 6);
    tip.style.left = `${x}px`;
    tip.style.top = `${y}px`;
    tip.dataset.side = side;
    requestAnimationFrame(() => tip?.classList.add('on'));
  }

  function hide() {
    clearTimeout(timer);
    tip?.remove();
    tip = null;
  }

  function enter() {
    clearTimeout(timer);
    timer = setTimeout(show, 400);
  }

  node.addEventListener('mouseenter', enter);
  node.addEventListener('mouseleave', hide);
  node.addEventListener('mousedown', hide);
  node.addEventListener('blur', hide);

  return {
    update(next: Options) {
      opts = next;
      if (tip && opts.enabled !== false) tip.textContent = opts.text;
      else hide();
    },
    destroy() {
      hide();
      node.removeEventListener('mouseenter', enter);
      node.removeEventListener('mouseleave', hide);
      node.removeEventListener('mousedown', hide);
      node.removeEventListener('blur', hide);
    },
  };
}
