/** Show help on hover or focus; a click pins it until dismissed. */
export function bindHelp(
  button: HTMLElement,
  text: HTMLElement,
  place: () => void,
) {
  const wrapper = button.parentElement!;
  let pinned = false;
  const show = (visible: boolean) => {
    text.hidden = !visible;
    button.setAttribute("aria-expanded", String(visible));
    if (visible) {
      text.showPopover();
      place();
    } else {
      text.hidePopover();
      pinned = false;
    }
  };
  wrapper.addEventListener("pointerenter", () => show(true));
  wrapper.addEventListener("pointerleave", () => {
    if (!pinned && document.activeElement !== button) show(false);
  });
  button.addEventListener("focus", () => show(true));
  button.addEventListener("blur", () => {
    if (!pinned) show(false);
  });
  button.addEventListener("click", () => {
    const pin = !pinned;
    show(pin);
    pinned = pin;
  });
  document.addEventListener("pointerdown", (event) => {
    if (!wrapper.contains(event.target as Node)) show(false);
  });
  return { show, visible: () => !text.hidden };
}
