// Hermes's hover marquee: a truncated title scrolls its hidden tail into view while hovered.
// The overflow is measured on pointerenter and the animation is armed only when there is some,
// so short titles keep their plain ellipsis. State lives in DOM attributes, not React state,
// so hovering never re-renders a memoized row. Pair with `.emp-marquee` > `.emp-marquee__inner`.

import type { PointerEvent } from "react";

const PX_PER_SECOND = 80;

export function armMarquee(event: PointerEvent<HTMLElement>): void {
  const el = event.currentTarget.querySelector<HTMLElement>(".emp-marquee") ?? event.currentTarget;
  const distance = el.scrollWidth - el.clientWidth;
  if (distance <= 2) return;
  // The keyframes travel for 65% of the cycle (10%→75%); scale so the travel itself runs at speed.
  el.style.setProperty("--emp-marquee-d", `${distance}px`);
  el.style.setProperty("--emp-marquee-t", `${Math.max(1, distance / PX_PER_SECOND / 0.65)}s`);
  el.dataset.marquee = "";
}

export function disarmMarquee(event: PointerEvent<HTMLElement>): void {
  const el = event.currentTarget.querySelector<HTMLElement>(".emp-marquee") ?? event.currentTarget;
  delete el.dataset.marquee;
}

export const marqueeHandlers = { onPointerEnter: armMarquee, onPointerLeave: disarmMarquee };
