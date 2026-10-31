import { BUTTONS } from './input.js';

/** Small live controller diagram: button chips, two sticks, two trigger bars. */
export class PadView {
  constructor(el) {
    el.classList.add('pad');
    el.innerHTML = `
      <div class="pad-sticks">
        <div class="stick"><div class="dot"></div></div>
        <div class="triggers"><div class="trigger"><div></div></div><div class="trigger"><div></div></div></div>
        <div class="stick"><div class="dot"></div></div>
      </div>
      <div class="pad-buttons">${BUTTONS.map((b) => `<span>${b}</span>`).join('')}</div>`;
    this.dots = el.querySelectorAll('.dot');
    this.triggers = el.querySelectorAll('.trigger > div');
    this.chips = el.querySelectorAll('.pad-buttons span');
  }

  render(s) {
    const place = (dot, x, y) => (dot.style.transform = `translate(${x * 18}px, ${y * 18}px)`);
    place(this.dots[0], s.lx, s.ly);
    place(this.dots[1], s.rx, s.ry);
    this.triggers[0].style.height = `${s.lt * 100}%`;
    this.triggers[1].style.height = `${s.rt * 100}%`;
    this.chips.forEach((c, i) => c.classList.toggle('on', ((s.buttons >>> i) & 1) === 1));
  }
}
