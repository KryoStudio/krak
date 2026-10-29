// `use:portal={'.content'}`: renders an element inside another container (e.g. a bar that must
// float over the scrolling page instead of scrolling with it).
export function portal(node: HTMLElement, selector: string) {
  (document.querySelector(selector) ?? document.body).append(node);
  return {
    destroy() {
      node.remove();
    },
  };
}
