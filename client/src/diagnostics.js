/** Key → value list with ok / bad / neutral colouring. */
export class Diagnostics {
  #rows = new Map();

  constructor(el) {
    this.el = el;
  }

  set(key, value, ok = null) {
    let row = this.#rows.get(key);
    if (!row) {
      row = document.createElement('div');
      row.innerHTML = '<dt></dt><dd></dd>';
      row.firstChild.textContent = key;
      this.el.append(row);
      this.#rows.set(key, row);
    }
    const dd = row.lastChild;
    dd.textContent = value;
    dd.className = ok === true ? 'ok' : ok === false ? 'bad' : '';
  }
}
