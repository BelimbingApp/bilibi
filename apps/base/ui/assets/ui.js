// Browser half of the shared feedback, dialog and tab-strip components
// (templates/feedback.html, dialog.html, tabs.html). It is the Bilimbi hook set
// -- Modal, TabStrip, FlashAutoDismiss and the connection banners -- ported to
// plain DOM, with the Phoenix LiveView bridge replaced by two small contracts:
//
//   * A component names its controller with data-hook="Name". This file mounts
//     it when the element enters the document (mounted), calls updated when its
//     subtree is patched, and calls destroyed once the element leaves. The keyed
//     patcher in live.js inserts and removes elements and reports the hooks
//     whose attributes it rewrote through BilibiUI.patched; it never calls a
//     hook itself. Other components register with BilibiUI.registerHook.
//   * A controller reports intent by raising a bubbling `bilibi:event`
//     CustomEvent whose detail is {event, ...values}; the transport turns it
//     into a server event. The server stays the one source of truth: a dialog
//     stays open until the server stops rendering it.
//
// The transport reports the socket by dispatching `bilibi:disconnected`
// (detail.kind "client" or "server") and `bilibi:connected` on window; the
// banners render hidden and this file reveals the matching pair.
(() => {
  'use strict';

  const hooks = {};
  const instances = new Map();
  let connection = null;

  const reducedMotion = () => matchMedia('(prefers-reduced-motion: reduce)').matches;

  function emit(el, event, detail = {}) {
    el.dispatchEvent(new CustomEvent('bilibi:event', {bubbles: true, detail: {event, ...detail}}));
  }

  // The motion Bilimbi's show/hide commands use: 300ms in, 200ms out, rising
  // from below on a phone and scaling from 95% on larger screens.
  function transition(el, entering) {
    if (reducedMotion() || !el.animate) return Promise.resolve();
    const wide = matchMedia('(min-width: 640px)').matches;
    const away = {opacity: 0, transform: wide ? 'scale(0.95)' : 'translateY(1rem)'};
    const rest = {opacity: 1, transform: 'none'};
    const animation = el.animate(entering ? [away, rest] : [rest, away], {
      duration: entering ? 300 : 200,
      easing: entering ? 'ease-out' : 'ease-in',
    });
    return animation.finished.catch(() => {});
  }

  function reveal(el) {
    if (!el.hidden) return;
    el.hidden = false;
    transition(el, true);
  }

  function conceal(el) {
    if (el.hidden) return;
    transition(el, false).then(() => { el.hidden = true; });
  }

  // Connection banners are bound to one kind of drop; a banner that appears
  // while the drop is on (a dialog opened during an outage) shows at once.
  function applyConnection(el) {
    if (connection !== null && el.dataset.connection === connection) reveal(el);
    else conceal(el);
  }

  window.addEventListener('bilibi:disconnected', (event) => {
    connection = event.detail?.kind === 'server' ? 'server' : 'client';
    document.querySelectorAll('[data-connection]').forEach(applyConnection);
  });
  window.addEventListener('bilibi:connected', () => {
    connection = null;
    document.querySelectorAll('[data-connection]').forEach(applyConnection);
  });

  // Dismissing a flash hides it and tells the server to forget it. A connection
  // banner or browser-owned preference flash only hides; neither is server state.
  function dismissFlash(el) {
    if (el.hidden) return;
    if (!el.hasAttribute('data-connection') && !el.hasAttribute('data-local-flash')) emit(el, el.dataset.event || 'clear-flash', {key: el.dataset.valueKey});
    conceal(el);
  }

  document.addEventListener('click', (event) => {
    const flash = event.target.closest('[data-flash]');
    if (flash) dismissFlash(flash);
  });

  // A submitting action link uses the same Origin/CSRF boundary as a form.
  // The owner returns an HTTP redirect; normal navigation then mounts that page.
  document.addEventListener('click', async (event) => {
    const link = event.target.closest('a[data-method]');
    if (!link || event.button !== 0 || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
    event.preventDefault();
    if (link.getAttribute('aria-busy') === 'true') return;
    const destination = new URL(link.dataset.to || link.href, location.href);
    if (destination.origin !== location.origin) return;
    const csrf = link.closest('[data-csrf]')?.dataset.csrf || document.querySelector('input[name=csrf]')?.value;
    link.setAttribute('aria-busy', 'true');
    try {
      const response = await fetch(destination, {method: link.dataset.method.toUpperCase(), headers: {'X-CSRF-Token': csrf || ''}, credentials: 'same-origin'});
      if (!response.ok) throw new Error('The request could not be sent. Try again.');
      location.assign(response.url);
    } catch (error) {
      let status = document.getElementById('action-link-status');
      if (!status) { status = document.createElement('p'); status.id = 'action-link-status'; status.setAttribute('role', 'alert'); link.after(status); }
      status.textContent = error.message;
      link.removeAttribute('aria-busy');
    }
  });

  // A control that names data-working shows that label, disabled and
  // aria-busy, from the click until the server replies (Bilimbi
  // phx-disable-with). Deferred one task so the transport and a form submit
  // still see the enabled control; settle() restores it when a reply patches
  // the control in place. A submit button whose form fails the browser's own
  // constraint validation raises no submit and gets no reply, so it stays
  // enabled, as phx-disable-with only acts on a submit that happens.
  const workingTimers = new WeakMap();
  document.addEventListener('click', (event) => {
    const button = event.target.closest('button[data-working]');
    if (!button || button.disabled) return;
    const form = button.form;
    if (form && button.type === 'submit' && !form.noValidate && !form.checkValidity()) return;
    const timer = setTimeout(() => {
      workingTimers.delete(button);
      if (!button.isConnected || button.disabled) return;
      button.dataset.workingFrom = button.textContent;
      button.textContent = button.dataset.working;
      button.disabled = true;
      button.setAttribute('aria-busy', 'true');
    }, 0);
    workingTimers.set(button, timer);
  });

  function settle(root = document) {
    // A local transport refusal can settle during the click/submit itself,
    // before the deferred working label. Cancel it so refusal cannot leave
    // the button disabled forever waiting for a reply that will never arrive.
    const pending = [...root.querySelectorAll('button[data-working]')];
    if (root instanceof Element && root.matches('button[data-working]')) pending.unshift(root);
    for (const button of pending) {
      clearTimeout(workingTimers.get(button));
      workingTimers.delete(button);
    }
    const buttons = [...root.querySelectorAll('button[data-working-from]')];
    if (root instanceof Element && root.matches('button[data-working-from]')) buttons.unshift(root);
    for (const button of buttons) {
      button.textContent = button.dataset.workingFrom;
      delete button.dataset.workingFrom;
      button.disabled = false;
      button.removeAttribute('aria-busy');
    }
  }

  // ---- Modal ---------------------------------------------------------------
  // Gives the server-rendered <dialog> real modal semantics. The server decides
  // whether a dialog exists: it renders the element while the workflow it hosts
  // is in progress and removes it when that workflow ends. The browser owns the
  // rest -- the top layer, an inert page behind, focus containment and Escape --
  // through showModal(). This hook bridges the two: it promotes the dialog on
  // mount, forwards every close request, and returns focus to the control that
  // opened the dialog once the server has removed it.
  //
  // The opener cannot be read from document.activeElement alone: a browser that
  // does not focus a <button> on click leaves focus on <body>. One capture-phase
  // pointerdown listener records the control the user activated instead. It is
  // armed here because the click that opens the first dialog of a page session
  // lands before any dialog exists to mount a hook. A key press forgets it, and
  // each dialog consumes it, so an old click cannot claim a dialog later opened
  // from the keyboard.
  const ACTIVATION_TARGETS = 'button, a[href], [tabindex]';
  let lastActivated = null;

  document.addEventListener('pointerdown', ({target}) => {
    lastActivated = target instanceof Element ? target.closest(ACTIVATION_TARGETS) : null;
  }, true);
  document.addEventListener('keydown', () => { lastActivated = null; }, true);

  hooks.Modal = {
    mounted() {
      this.opener = this.openerFrom(lastActivated) || this.openerFrom(document.activeElement);
      lastActivated = null;

      // The server renders `open` so a later patch never strips the attribute
      // and closes the dialog under the user. An open non-modal dialog cannot
      // be promoted in place, so it is reopened as modal before anything paints.
      if (this.el.open) this.el.close();
      this.el.showModal();
      // A confirmation starts on Cancel even when its body contains a reason field.
      if (this.el.getAttribute('role') === 'alertdialog') {
        const cancel = Array.from(this.el.querySelectorAll('button[data-event]'))
          .find(button => button.dataset.event === this.el.dataset.cancel);
        if (cancel) cancel.focus();
      }

      // Escape asks to close. The dialog stays open until the server removes
      // it, so there is one source of truth for whether the workflow is live.
      this.el.addEventListener('cancel', (event) => {
        event.preventDefault();
        this.requestClose();
      });
      // A close the browser refuses to cancel (a repeated Escape) or a script
      // close still tells the server, so the two cannot disagree for long.
      this.el.addEventListener('close', () => {
        if (!this.el.open) this.requestClose();
      });
    },

    updated() {
      // Moving a retained dialog during a patch drops it out of the top layer.
      // Restore modality without losing the reason field's focus or draft.
      if (this.el.open && !this.el.matches(':modal')) {
        const focused = this.el.contains(document.activeElement) ? document.activeElement : null;
        this.el.close();
        this.el.showModal();
        if (focused) focused.focus();
      }
    },

    destroyed() {
      const opener = this.opener;
      if (opener && opener.isConnected && typeof opener.focus === 'function') opener.focus();
    },

    openerFrom(candidate) {
      if (!candidate || candidate === document.body) return null;
      if (!candidate.isConnected || this.el.contains(candidate)) return null;
      return typeof candidate.focus === 'function' ? candidate : null;
    },

    requestClose() {
      const cancel = this.el.getAttribute('data-cancel');
      if (cancel) this.emit(cancel);
    },
  };

  // ---- TabStrip ------------------------------------------------------------
  // Keeps a tab strip on one line. Tabs that do not fit scroll inside the strip
  // instead of widening the page. The edge buttons are a pointer affordance:
  // they stay out of tab order, and focusing a tab scrolls it clear of the edge
  // that would cover it. The buttons and the fade exist only while a tab
  // actually sits outside the strip.
  const EDGE = 32;

  hooks.TabStrip = {
    mounted() {
      this.scroller = this.el.querySelector('[data-tab-scroller]');
      this.start = this.el.querySelector("[data-tab-scroll='start']");
      this.end = this.el.querySelector("[data-tab-scroll='end']");

      this.onScroll = () => this.sync();
      this.onFocus = (event) => {
        const tab = event.target.closest('[data-tab]');
        if (!tab || !this.scroller.contains(tab)) return;
        revealTab(this.scroller, tab);
        this.sync();
      };
      // A click would focus the button and drop the tabs out of the next Tab
      // press. Preventing the mousedown focus keeps keyboard order on the tabs.
      this.onMouseDown = (event) => {
        const button = event.target.closest('[data-tab-scroll]');
        if (button && this.el.contains(button)) event.preventDefault();
      };
      this.onClick = (event) => {
        const button = event.target.closest('[data-tab-scroll]');
        if (!button || !this.el.contains(button)) return;
        revealNeighbor(this.scroller, button.dataset.tabScroll === 'start' ? 'start' : 'end');
        this.sync();
      };

      this.scroller.addEventListener('scroll', this.onScroll, {passive: true});
      this.el.addEventListener('focusin', this.onFocus);
      this.el.addEventListener('mousedown', this.onMouseDown);
      this.el.addEventListener('click', this.onClick);

      this.resize = new ResizeObserver(() => this.sync());
      this.resize.observe(this.scroller);

      const current = this.scroller.querySelector("[aria-current='page']");
      if (current) revealTab(this.scroller, current);
      this.sync();
    },

    updated() {
      const current = this.scroller.querySelector("[aria-current='page']");
      if (current) revealTab(this.scroller, current);
      this.sync();
    },

    destroyed() {
      this.resize.disconnect();
    },

    sync() {
      paint(this.scroller, this.start, this.end);
    },
  };

  const laidOut = (scroller) => scroller.clientWidth > 0 && scroller.scrollWidth > 0;
  const span = (tab) => ({start: tab.offsetLeft, end: tab.offsetLeft + tab.offsetWidth});

  function view(scroller) {
    return {
      start: scroller.scrollLeft + EDGE,
      end: scroller.scrollLeft + scroller.clientWidth - EDGE,
      max: Math.max(0, scroller.scrollWidth - scroller.clientWidth),
    };
  }

  function revealTab(scroller, tab) {
    if (!laidOut(scroller)) return;
    const box = span(tab);
    const visible = view(scroller);
    if (box.start < visible.start) {
      scroller.scrollLeft = Math.max(0, box.start - EDGE);
    } else if (box.end > visible.end) {
      scroller.scrollLeft = Math.min(visible.max, box.end - scroller.clientWidth + EDGE);
    }
  }

  function revealNeighbor(scroller, toward) {
    if (!laidOut(scroller)) return;
    const visible = view(scroller);
    const tabs = [...scroller.querySelectorAll('[data-tab]')];
    const target = toward === 'end'
      ? tabs.find((tab) => span(tab).end > visible.end + 1)
      : [...tabs].reverse().find((tab) => span(tab).start < visible.start - 1);
    if (target) revealTab(scroller, target);
  }

  const fadeEnd = 'linear-gradient(to left, transparent, #000 2rem)';
  const fadeStart = 'linear-gradient(to right, transparent, #000 2rem)';
  const fadeBoth = 'linear-gradient(to right, transparent, #000 2rem, #000 calc(100% - 2rem), transparent)';

  function paint(scroller, start, end) {
    if (!laidOut(scroller)) {
      start.hidden = true;
      end.hidden = true;
      mask(scroller, '');
      return;
    }
    const max = scroller.scrollWidth - scroller.clientWidth;
    const overflow = max > 1;
    const showStart = overflow && scroller.scrollLeft > 1;
    const showEnd = overflow && scroller.scrollLeft < max - 1;
    start.hidden = !showStart;
    end.hidden = !showEnd;
    mask(scroller, showStart && showEnd ? fadeBoth : showStart ? fadeStart : showEnd ? fadeEnd : '');
  }

  function mask(scroller, value) {
    scroller.style.maskImage = value;
    scroller.style.webkitMaskImage = value;
  }

  // ---- FlashAutoDismiss ----------------------------------------------------
  // Dismisses a transient flash after the delay the server wrote on it. The
  // flash group attaches this hook only to success messages; info, warning and
  // error never carry it, so nothing a person must act on can disappear on a
  // timer. A timed dismissal and a clicked one leave the same state behind.
  hooks.FlashAutoDismiss = {
    mounted() {
      this.delay = Number(this.el.dataset.autoDismissMs);
      if (!(this.delay > 0)) return;
      this.timer = setTimeout(() => {
        this.timer = null;
        dismissFlash(this.el);
      }, this.delay);
    },

    destroyed() {
      clearTimeout(this.timer);
    },
  };

  // ---- Hook lifecycle ------------------------------------------------------
  // `fresh` collects the hooks mounted in one pass so a patch that inserts a
  // control never follows its `mounted` with an `updated` for the same change.
  function mount(el, fresh) {
    if (instances.has(el)) return;
    const hook = hooks[el.dataset.hook];
    if (!hook) return;
    const instance = Object.create(hook);
    instance.el = el;
    instance.hookName = el.dataset.hook;
    instance.emit = (event, detail) => emit(el, event, detail);
    instances.set(el, instance);
    if (fresh) fresh.add(el);
    if (instance.mounted) instance.mounted();
  }

  function scan(root, fresh) {
    if (root.nodeType !== Node.ELEMENT_NODE) return;
    const matching = (selector, visit) => {
      if (root.matches(selector)) visit(root);
      root.querySelectorAll(selector).forEach(visit);
    };
    matching('[data-hook]', (el) => mount(el, fresh));
    matching('[data-connection]', applyConnection);
  }

  function sweep() {
    for (const [el, instance] of instances) {
      // A patch can also drop or rename an element's data-hook in place.
      if (el.isConnected && el.dataset.hook === instance.hookName) continue;
      instances.delete(el);
      if (instance.destroyed) instance.destroyed();
    }
  }

  // One pass over mutation records: mount what arrived, destroy what left, then
  // call `updated` once per hook whose subtree changed. The live patcher adds
  // the hooks whose attributes it rewrote, which the observer does not watch,
  // and drains the observer first so the two never report the same patch twice.
  function process(records, touched = []) {
    const fresh = new Set();
    const patched = new Set(touched);
    let removed = false;
    for (const record of records) {
      record.addedNodes.forEach((node) => scan(node, fresh));
      if (record.removedNodes.length) removed = true;
      const owner = record.target.nodeType === Node.ELEMENT_NODE ? record.target : record.target.parentElement;
      const hooked = owner && owner.closest('[data-hook]');
      if (hooked) patched.add(hooked);
    }
    if (removed || touched.length || touched.size) sweep();
    // A patch can add data-hook to an element already on the page (a secret
    // becoming stored); the patcher reports it, and it mounts here.
    for (const el of patched) if (!instances.has(el) && el.isConnected) mount(el, fresh);
    for (const el of patched) {
      if (fresh.has(el)) continue;
      const instance = instances.get(el);
      if (instance && instance.updated && el.isConnected) instance.updated();
    }
  }

  const observer = new MutationObserver((records) => process(records));

  function start() {
    scan(document.documentElement);
    observer.observe(document.documentElement, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: ['aria-current'],
    });
  }

  window.BilibiUI = {
    emit,
    settle,
    // The live patcher reports the hooks whose attributes or subtree it rewrote.
    patched(touched) {
      process(observer.takeRecords(), touched);
    },
    registerHook(name, hook) {
      hooks[name] = hook;
      document.querySelectorAll(`[data-hook="${name}"]`).forEach((el) => mount(el));
    },
  };

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start);
  else start();
})();
