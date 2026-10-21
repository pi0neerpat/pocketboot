// Maps a `ViewModel` to DOM. The live region is rebuilt only when the state's
// meaning changes; a percent tick only rewrites the status line, so screen
// readers are not re-read the headline on every progress event (R54).

import type { Button, Icon, ViewModel } from "./copy";
import type { Action } from "./types";

const SVG_NS = "http://www.w3.org/2000/svg";

const ICON_PATHS: Record<Icon, string> = {
  radio: "M7 3h10a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2zm1 3v5h8V6zm2 9a1.5 1.5 0 1 0 0 .01zm4 0a1.5 1.5 0 1 0 0 .01z",
  search: "M10 4a6 6 0 1 0 3.5 10.9l4.3 4.3 1.4-1.4-4.3-4.3A6 6 0 0 0 10 4zm0 2a4 4 0 1 1 0 8 4 4 0 0 1 0-8z",
  save: "M5 3h11l3 3v13a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2zm2 2v4h8V5zm5 7a3 3 0 1 0 0 6 3 3 0 0 0 0-6z",
  check: "M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20zm-1.2 14.2-4-4 1.4-1.4 2.6 2.6 5.6-5.6 1.4 1.4z",
  warning: "M12 2 1 21h22zm-1 7h2v6h-2zm0 8h2v2h-2z",
  stop: "M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20zM7 11h10v2H7z",
};

function el<K extends keyof HTMLElementTagNameMap>(tag: K, cls?: string, text?: string): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function icon(name: Icon): SVGSVGElement {
  const svg = document.createElementNS(SVG_NS, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  svg.classList.add("icon", `icon-${name}`);
  const path = document.createElementNS(SVG_NS, "path");
  path.setAttribute("d", ICON_PATHS[name]);
  svg.append(path);
  return svg;
}

function button(b: Button, cls: string, onAction: (a: Action) => void): HTMLButtonElement {
  const btn = el("button", cls, b.label);
  btn.type = "button";
  if (b.ariaLabel) btn.setAttribute("aria-label", b.ariaLabel);
  btn.disabled = b.disabled ?? false;
  btn.addEventListener("click", () => {
    if (btn.disabled) return;
    // R31: disabled until the next `state-changed` rebuilds the screen.
    btn.disabled = true;
    onAction(b.action);
  });
  return btn;
}

export class Screen {
  private key = "";
  private status: HTMLElement | null = null;

  constructor(
    private root: HTMLElement,
    private onAction: (a: Action) => void,
  ) {}

  /// `key` identifies the state's meaning; an unchanged key only updates the status line.
  show(vm: ViewModel, key: string): void {
    if (key === this.key && this.status) {
      this.status.textContent = vm.status ?? "";
      return;
    }
    const entered = key.split("|")[0] !== this.key.split("|")[0];
    this.key = key;
    this.root.replaceChildren();

    this.root.append(icon(vm.icon));
    const live = el("div", "live");
    live.setAttribute("aria-live", vm.assertive ? "assertive" : "polite");
    const headline = el("h1", "headline", vm.headline);
    headline.tabIndex = -1;
    live.append(headline);
    if (vm.body) live.append(el("p", "body", vm.body));
    if (vm.list) {
      const ul = el("ul", "devices");
      for (const item of vm.list) ul.append(el("li", undefined, item));
      live.append(ul);
    }
    if (vm.steps) {
      const ol = el("ol", "steps");
      for (const step of vm.steps) ol.append(el("li", undefined, step));
      live.append(ol);
    }
    if (vm.after) live.append(el("p", "body", vm.after));
    this.root.append(live);

    let primary: HTMLButtonElement | null = null;
    if (vm.primary) {
      primary = button(vm.primary, "primary", this.onAction);
      this.root.append(primary);
    }
    if (vm.secondary) this.root.append(button(vm.secondary, "secondary", this.onAction));

    this.status = el("p", vm.spinner ? "status spinning" : "status", vm.status ?? "");
    this.root.append(this.status);
    if (vm.footnote) this.root.append(el("p", "footnote", vm.footnote));

    // R30: focus the primary on entry, or the headline when there is none.
    if (entered || primary) {
      if (primary && !primary.disabled) primary.focus();
      else headline.focus();
    }
  }
}
