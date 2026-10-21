// Authenticated shell chrome (templates/shell.html, framed.html): Bilimbi's
// AppShell and ShellControls hooks (apps/web/assets/js/app_shell.js,
// shell_controls.js) on the BilibiUI hook lifecycle. Navigation, capabilities,
// account and scope stay server-rendered; this file owns only what the server
// cannot: the desktop show/hide and width of the menu, branch expansion, the
// mobile drawer with Escape/backdrop close and focus return, pins, the account
// and time-display disclosures, theme projection and preference feedback.
//
// Theme, time-display mode and pins are the signed-in account's own, kept by
// Core User: the server renders them (`data-theme-choice`, `data-display-mode`,
// `data-pins`) and this file saves a choice through POST /preferences and the
// pins through /api/pins. Nothing about them is kept in the browser, so every
// device of the account agrees.
(() => {
  'use strict';

  const DESKTOP = '(min-width: 1024px)';
  const WORKSPACE = '/workspace';
  const MIN_WIDTH = 180;
  const MAX_WIDTH = 360;
  const DEFAULT_WIDTH = 240;
  const NAV_EXPANSION_STORAGE = 'sidebarExpandedBranches';
  const THEMES = ['light', 'dark', 'system'];
  const MODE_LABELS = {company: 'Company', local: 'Local', utc: 'UTC'};
  const FOCUSABLE = 'a[href], button:not([disabled]), [tabindex]:not([tabindex="-1"])';

  const read = (key) => { try { return window.localStorage.getItem(key); } catch { return null; } };
  const write = (key, value) => { try { window.localStorage.setItem(key, value); } catch { /* private mode */ } };

  // An explicit light/dark choice stamps data-theme (the server stamps it on
  // first paint); "system" stamps nothing so prefers-color-scheme governs
  // (assets/css/app.css).
  function projectTheme(theme) {
    if (theme === 'light' || theme === 'dark') {
      if (document.documentElement.dataset.theme !== theme) document.documentElement.dataset.theme = theme;
    } else if ('theme' in document.documentElement.dataset) {
      delete document.documentElement.dataset.theme;
    }
  }

  // The signed-in account's write requests: same-origin JSON with its CSRF token.
  function accountRequest(path, options = {}) {
    const csrf = document.querySelector('#app-shell input[name="csrf"]')?.value || '';
    const headers = {Accept: 'application/json', ...(options.headers || {})};
    if (options.method && options.method !== 'GET') {
      headers['Content-Type'] = 'application/json';
      headers['X-CSRF-Token'] = csrf;
    }
    return fetch(path, {...options, headers, credentials: 'same-origin'});
  }

  // One page as a workspace tree leaf, escaped as Bilimbi's Tiling layout does.
  const leaf = (page) => page.replace(/[%(),]/g, (char) => `%${char.charCodeAt(0).toString(16).toUpperCase()}`);

  // ---- Disclosures and display preferences (Bilimbi ShellControls) ----------
  class ShellControls {
    constructor(shell) {
      this.shell = shell;
      this.root = shell.el;
      this.pending = false;
      this.openPanelId = null;
      this.message = null;
      this.messageError = false;
      this.onClick = (event) => this.click(event);
      this.onKey = (event) => this.key(event);
      document.addEventListener('click', this.onClick);
      document.addEventListener('keydown', this.onKey);
      this.apply();
    }

    destroy() {
      clearTimeout(this.timer);
      this.abort?.abort();
      document.removeEventListener('click', this.onClick);
      document.removeEventListener('keydown', this.onKey);
    }

    // The bell's dropdown is Core's `shell.notifications` embed; its contents
    // follow the server while open/closed stays here, like the other panels.
    panels() { return [...this.root.querySelectorAll('[data-account-panel], [data-timezone-panel], [data-notifications-panel]')]; }
    openPanel() { return this.panels().find((panel) => panel.id === this.openPanelId) || null; }
    trigger(panel) { return this.root.querySelector(`[aria-controls="${panel.id}"]`); }
    close(panel, restore = false) {
      if (this.openPanelId === panel.id) this.openPanelId = null;
      this.apply();
      if (restore) this.trigger(panel)?.focus();
    }
    closeAll(restore = false) {
      const open = this.openPanel();
      if (open) this.close(open, restore);
    }
    toggle(button) {
      const panel = document.getElementById(button.getAttribute('aria-controls'));
      if (!panel) return;
      const opening = this.openPanelId !== panel.id;
      this.openPanelId = opening ? panel.id : null;
      this.apply();
      if (opening) panel.querySelector('a[href], button:not([disabled])')?.focus();
    }
    click(event) {
      const button = event.target.closest('button');
      if (button && this.root.contains(button)) {
        if (button.matches('[data-account-toggle], [data-timezone-toggle], [data-notifications-toggle]')) {
          this.toggle(button);
          return;
        }
        // Opening a notification closes the bell (Bilimbi's `visit`); the
        // transport still sends the intent from its own listener.
        if (button.matches('[data-notifications-panel] [data-event="visit"]')) {
          this.closeAll(true);
          return;
        }
        if (button.matches('[data-preference-kind]')) {
          this.save(button.dataset.preferenceKind, button.dataset.preferenceValue, button);
          return;
        }
      }
      const open = this.openPanel();
      if (open && !open.contains(event.target)) this.close(open);
    }
    key(event) {
      const panel = this.openPanel();
      if (!panel) return;
      if (event.key === 'Escape') {
        event.preventDefault();
        event.stopImmediatePropagation();
        this.close(panel, true);
      } else if (event.key === 'Tab') {
        // Disclosures use the normal tab order. Close after focus leaves them.
        setTimeout(() => {
          if (!panel.contains(document.activeElement) && document.activeElement !== this.trigger(panel)) this.close(panel);
        }, 0);
      }
    }
    feedback(message, error = false) {
      this.message = message;
      this.messageError = error;
      this.apply();
    }
    finish(restore) {
      clearTimeout(this.timer);
      this.pending = false;
      this.apply();
      restore?.focus();
    }
    save(kind, value, origin) {
      if (this.pending) return;
      const open = this.openPanel();
      const restore = (open && open.contains(origin) && this.trigger(open)) || origin;
      if (kind === 'theme' && !THEMES.includes(value)) return;
      this.pending = true;
      this.feedback(kind === 'theme' ? 'Saving theme…' : 'Saving display preference…');
      this.abort = new AbortController();
      this.timer = setTimeout(() => {
        this.abort.abort();
        this.feedback('Save could not be confirmed. Reload to check your saved preference.', true);
      }, 12000);
      accountRequest('/preferences', {method: 'POST', signal: this.abort.signal, body: JSON.stringify({kind, value})})
        .then((response) => (response.ok ? response.json() : Promise.reject(response.status)))
        .then((reply) => {
          this.root.dataset.themeChoice = reply.theme;
          this.root.dataset.displayMode = reply.mode;
          if (kind === 'timezone') window.dispatchEvent(new CustomEvent('bilibi:display-mode', {detail: {mode: reply.mode}}));
          this.feedback(kind === 'theme' ? 'Theme saved.' : 'Time display saved.');
          this.closeAll();
        })
        .catch(() => {
          if (!this.abort.signal.aborted) {
            this.feedback(`Could not save ${kind === 'theme' ? 'theme' : 'display preference'}. Your previous choice is still selected. Try again.`, true);
          }
        })
        .finally(() => this.finish(restore));
    }
    apply() {
      const theme = this.root.dataset.themeChoice || 'system';
      projectTheme(theme);
      const mode = this.root.dataset.displayMode;
      for (const button of this.root.querySelectorAll('[data-preference-kind]')) {
        button.disabled = this.pending;
        const current = button.dataset.preferenceKind === 'theme' ? theme : mode;
        button.setAttribute('aria-pressed', String(button.dataset.preferenceValue === current));
      }
      const label = MODE_LABELS[mode];
      // Text writes are child-list mutations, which ui.js reports back as
      // `updated`; writing only on change keeps that from looping.
      if (label) {
        this.root.querySelectorAll('[data-mode-label]').forEach((el) => {
          if (el.textContent !== label) el.textContent = label;
        });
        this.root.querySelector('[data-timezone-toggle]')?.setAttribute('title', `Time display: ${label}`);
      }
      for (const panel of this.panels()) {
        const open = panel.id === this.openPanelId;
        panel.hidden = !open;
        this.trigger(panel)?.setAttribute('aria-expanded', String(open));
      }
      const region = this.root.querySelector('#app-preference-feedback');
      if (region) {
        region.hidden = this.message === null;
        if (region.textContent !== (this.message ?? '')) region.textContent = this.message ?? '';
        region.classList.toggle('text-danger', this.messageError);
      }
    }
  }

  // ---- Sidebar, drawer and pins (Bilimbi AppShell) --------------------------
  const AppShell = {
    mounted() {
      this.root = this.el;
      this.toggle = this.el.querySelector('#app-sidebar-toggle');
      this.sidebar = this.el.querySelector('#app-sidebar');
      this.backdrop = this.el.querySelector('#app-sidebar-backdrop');
      this.content = this.el.querySelector('#app-content');
      this.topbarMain = this.el.querySelector('#app-topbar-main');
      this.statusbar = this.el.querySelector('#app-statusbar');
      this.drag = this.el.querySelector('#app-sidebar-drag');
      this.pinned = this.el.querySelector('#app-pinned');
      this.pinnedItems = this.el.querySelector('#app-pinned-items');
      this.pinnedButton = this.el.querySelector('#app-pinned-button');
      this.pinnedAnnouncement = this.el.querySelector('#app-pinned-announcement');
      this.impersonating = this.el.dataset.impersonating === 'true';
      this.servedRoutes = this.readServedRoutes();
      this.mq = window.matchMedia(DESKTOP);
      this.hidden = read('sidebarHidden') === '1';
      this.width = this.readWidth();
      this.expandedBranches = this.readExpandedBranches();
      this.pinnedEntries = this.readRenderedPins();
      this.pinning = false;
      this.drawerOpen = false;
      this.lastFocus = null;
      this.dragging = false;
      this.draggedPinnedKey = null;
      this.dropPinnedKey = null;

      this.onToggle = () => this.toggleSidebar();
      this.onBackdrop = () => this.closeDrawer();
      this.onKey = (event) => this.onGlobalKey(event);
      this.onMq = () => this.onViewportChange();
      this.onNav = (event) => this.onSidebarClick(event);
      this.onNavSearch = (event) => { if (event.target.id === 'app-nav-search') this.applyNavFilter(); };
      this.onDragStart = (event) => this.startDrag(event);
      this.onDragMove = (event) => this.moveDrag(event);
      this.onDragEnd = () => this.endDrag();
      this.onDragKey = (event) => this.keyDrag(event);
      this.onPinnedDragStart = (event) => this.startPinnedDrag(event);
      this.onPinnedDragOver = (event) => this.overPinnedDrag(event);
      this.onPinnedDrop = (event) => this.dropPinnedDrag(event);
      this.onPinnedDragEnd = () => this.endPinnedDrag();

      this.toggle?.addEventListener('click', this.onToggle);
      this.backdrop?.addEventListener('click', this.onBackdrop);
      this.root.addEventListener('click', this.onNav);
      this.root.addEventListener('input', this.onNavSearch);
      this.drag?.addEventListener('mousedown', this.onDragStart);
      this.drag?.addEventListener('keydown', this.onDragKey);
      this.pinnedItems?.addEventListener('dragstart', this.onPinnedDragStart);
      this.pinnedItems?.addEventListener('dragover', this.onPinnedDragOver);
      this.pinnedItems?.addEventListener('drop', this.onPinnedDrop);
      this.pinnedItems?.addEventListener('dragend', this.onPinnedDragEnd);
      window.addEventListener('keydown', this.onKey);
      this.mq.addEventListener('change', this.onMq);
      this.controls = new ShellControls(this);
      this.apply();
    },

    // A live patch rewrites the server-rendered state (hidden panels, sidebar
    // data attributes); the browser's own state is re-applied over it.
    updated() {
      this.controls?.apply();
      this.apply();
    },

    destroyed() {
      this.controls?.destroy();
      this.toggle?.removeEventListener('click', this.onToggle);
      this.backdrop?.removeEventListener('click', this.onBackdrop);
      this.root?.removeEventListener('click', this.onNav);
      this.root?.removeEventListener('input', this.onNavSearch);
      this.drag?.removeEventListener('mousedown', this.onDragStart);
      this.drag?.removeEventListener('keydown', this.onDragKey);
      this.pinnedItems?.removeEventListener('dragstart', this.onPinnedDragStart);
      this.pinnedItems?.removeEventListener('dragover', this.onPinnedDragOver);
      this.pinnedItems?.removeEventListener('drop', this.onPinnedDrop);
      this.pinnedItems?.removeEventListener('dragend', this.onPinnedDragEnd);
      window.removeEventListener('mousemove', this.onDragMove);
      window.removeEventListener('mouseup', this.onDragEnd);
      window.removeEventListener('keydown', this.onKey);
      this.mq?.removeEventListener('change', this.onMq);
    },

    desktop() { return this.mq.matches; },

    readWidth() {
      const stored = Number.parseInt(read('sidebarWidth'), 10);
      if (Number.isFinite(stored) && stored >= MIN_WIDTH && stored <= MAX_WIDTH) return stored;
      return DEFAULT_WIDTH;
    },

    readExpandedBranches() {
      try {
        const stored = JSON.parse(read(NAV_EXPANSION_STORAGE) ?? '{}');
        return stored && typeof stored === 'object' && !Array.isArray(stored) ? stored : {};
      } catch {
        return {};
      }
    },

    readServedRoutes() {
      try {
        const routes = JSON.parse(this.root.dataset.servedRoutes ?? '[]');
        return Array.isArray(routes) ? routes.filter((route) => typeof route === 'string') : [];
      } catch {
        return [];
      }
    },

    // The account's pins, rendered by the server into `data-pins` as
    // {id, label, url, icon, sort_order}, in the order the person arranged.
    readRenderedPins() {
      try {
        const parsed = JSON.parse(this.root.dataset.pins ?? '[]');
        return this.acceptServerPins(parsed);
      } catch {
        return [];
      }
    },

    acceptServerPins(pins) {
      if (!Array.isArray(pins)) return [];
      return pins.map((pin) => this.normalizePin(pin)).filter((pin) => pin && this.isServedUrl(pin.url));
    },

    normalizePin(pin) {
      if (!pin || typeof pin !== 'object' || Array.isArray(pin)) return null;
      const url = this.normalizePinnedUrl(pin.url);
      const label = typeof pin.label === 'string' ? pin.label.trim() : '';
      return label && url && Number.isInteger(pin.id) ? {pinId: pin.id, label, url, icon: pin.icon || null} : null;
    },

    normalizePinnedUrl(url) {
      if (typeof url !== 'string') return null;
      try {
        const parsed = new URL(url, window.location.origin);
        if (parsed.origin !== window.location.origin) return null;
        const path = parsed.pathname.replace(/\/+$/, '') || '/';
        const query = new URLSearchParams(parsed.search);
        query.sort();
        const search = query.toString();
        return search ? `${path}?${search}` : path;
      } catch {
        return null;
      }
    },

    // A pin is identified by its page, as the server does.
    pinnedItemKey(item) { return item?.url ?? null; },

    pinnedUrl(item) { return item?.url ?? null; },

    pinRequest(path, options = {}) { return accountRequest(path, options); },

    // Discovered page routes use `{param}` segments.
    isServedUrl(url) {
      const normalized = this.normalizePinnedUrl(url);
      if (!normalized) return false;
      const actual = normalized.split('?', 1)[0].split('/').filter(Boolean);
      return this.servedRoutes.some((route) => {
        const pattern = route.split('/').filter(Boolean);
        return pattern.length === actual.length &&
          pattern.every((segment, index) => segment.startsWith('{') || segment === actual[index]);
      });
    },

    startDrag(event) {
      if (!this.desktop() || event.button !== 0) return;
      event.preventDefault();
      this.dragging = true;
      this.dragStartX = event.clientX;
      this.dragStartWidth = this.width;
      document.documentElement.style.cursor = 'col-resize';
      document.documentElement.style.userSelect = 'none';
      window.addEventListener('mousemove', this.onDragMove);
      window.addEventListener('mouseup', this.onDragEnd);
    },

    moveDrag(event) {
      if (!this.dragging) return;
      const next = this.dragStartWidth + (event.clientX - this.dragStartX);
      this.width = Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, next));
      this.apply();
    },

    endDrag() {
      if (!this.dragging) return;
      this.dragging = false;
      document.documentElement.style.cursor = '';
      document.documentElement.style.userSelect = '';
      window.removeEventListener('mousemove', this.onDragMove);
      window.removeEventListener('mouseup', this.onDragEnd);
      write('sidebarWidth', String(this.width));
    },

    // The separator is focusable, so arrow keys resize it as a drag does.
    keyDrag(event) {
      const step = {ArrowLeft: -16, ArrowRight: 16}[event.key];
      if (!step || !this.desktop()) return;
      event.preventDefault();
      this.width = Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, this.width + step));
      write('sidebarWidth', String(this.width));
      this.apply();
    },

    toggleSidebar() {
      if (this.desktop()) {
        this.hidden = !this.hidden;
        write('sidebarHidden', this.hidden ? '1' : '0');
      } else if (this.drawerOpen) {
        this.closeDrawer();
        return;
      } else {
        this.lastFocus = document.activeElement;
        this.drawerOpen = true;
        this.apply();
        this.focusSidebar();
        return;
      }
      this.apply();
    },

    closeDrawer() {
      if (this.desktop() || !this.drawerOpen) return;
      this.controls?.closeAll();
      this.drawerOpen = false;
      this.apply();
      const restore = this.lastFocus || this.toggle;
      this.lastFocus = null;
      restore?.focus?.();
    },

    onViewportChange() {
      this.setPinnedMenu(false);
      if (this.desktop()) {
        this.drawerOpen = false;
        this.lastFocus = null;
      }
      this.controls?.closeAll();
      this.apply();
    },

    onGlobalKey(event) {
      if (event.key === 'Escape') {
        if (this.pinnedMenuOpen()) {
          this.setPinnedMenu(false);
          this.pinnedButton?.focus();
          return;
        }
        if (this.controls?.panels().some((panel) => !panel.hidden)) return;
        this.closeDrawer();
        return;
      }
      if (event.key !== 'Tab' || this.desktop() || !this.drawerOpen) return;
      const nodes = this.focusable();
      if (nodes.length === 0) return;
      const first = nodes[0];
      const last = nodes[nodes.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    },

    pinnedMenuOpen() { return this.pinnedItems?.dataset.open === 'true'; },

    // Below `lg` the pinned items are a dropdown; from `lg` the same element is
    // a row and this state is inert (app.css shows it either way).
    setPinnedMenu(open) {
      if (!this.pinnedItems || !this.pinnedButton) return;
      if (open) {
        this.controls?.closeAll();
        this.pinnedItems.dataset.open = 'true';
      } else {
        delete this.pinnedItems.dataset.open;
      }
      this.pinnedButton.setAttribute('aria-expanded', open ? 'true' : 'false');
    },

    onSidebarClick(event) {
      if (this.pinnedButton?.contains(event.target)) {
        this.setPinnedMenu(!this.pinnedMenuOpen());
        return;
      }
      if (this.pinnedMenuOpen() &&
          (!this.pinned.contains(event.target) || event.target.closest('.app-pinned-link, .app-pinned-tile'))) {
        this.setPinnedMenu(false);
      }
      const unpin = event.target.closest('[data-nav-unpin]');
      if (unpin && this.root.contains(unpin)) {
        event.preventDefault();
        this.removePinnedItem(unpin.dataset.navUnpin);
        return;
      }
      const pin = event.target.closest('[data-nav-pin]');
      if (pin && this.root.contains(pin)) {
        event.preventDefault();
        this.togglePinnedItem(this.pinFromControl(pin));
        return;
      }
      const tile = event.target.closest('[data-nav-tile]');
      if (tile && this.root.contains(tile)) this.retargetTileLink(tile, event);
      if (!this.sidebar?.contains(event.target)) return;
      const toggle = event.target.closest('[data-nav-toggle]');
      if (toggle && this.sidebar.contains(toggle)) {
        event.preventDefault();
        this.toggleNavBranch(toggle.closest('[data-nav-branch]'));
        return;
      }
      if (this.desktop() || !event.target.closest('a[href]')) return;
      this.closeDrawer();
    },

    // "Open in a tile": a plain link to the page, retargeted on a wide screen
    // to the workspace with the current page in `t` and the clicked one in
    // `open`, as Bilimbi does. Until the workspace route is served the link
    // stays a plain link, so it never leads to a missing page. From the
    // workspace it is a patch of its own address (live.js `bilibi:patch`), so
    // the tiles on screen keep their frames; from a page it opens the
    // workspace. The tree is never built here: `t` is only ever the current
    // address, and the server does the splitting.
    retargetTileLink(link, event) {
      const page = link.dataset.navTile;
      if (!page || !this.desktop() || !this.isServedUrl(WORKSPACE)) return;
      const {pathname, search} = window.location;
      const workspace = pathname === WORKSPACE || pathname.startsWith(`${WORKSPACE}/`);
      const params = workspace
        ? new URLSearchParams(search)
        : new URLSearchParams({t: leaf(pathname + search), inplace: '1'});
      params.set('open', page);
      link.setAttribute('href', `${workspace ? pathname : WORKSPACE}?${params}`);
      if (!workspace || event?.defaultPrevented || event?.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      const patch = new CustomEvent('bilibi:patch', {cancelable: true, detail: {path: pathname, params: Object.fromEntries(params)}});
      if (!document.dispatchEvent(patch)) event.preventDefault();
    },

    toggleNavBranch(branch) {
      if (!branch) return;
      const expanded = branch.dataset.navExpanded !== 'true';
      // While the menu is filtered a toggle is not saved, so clearing the
      // filter brings back the expansion the reader had.
      if (!this.navQuery()) {
        this.expandedBranches[branch.dataset.navBranch] = expanded;
        write(NAV_EXPANSION_STORAGE, JSON.stringify(this.expandedBranches));
      }
      this.setNavBranch(branch, expanded);
    },

    // A branch's own expansion: the reader's saved choice, else the server's.
    branchExpanded(branch) {
      const id = branch.dataset.navBranch;
      return Object.hasOwn(this.expandedBranches, id)
        ? this.expandedBranches[id]
        : branch.dataset.navDefaultExpanded === 'true';
    },

    applyNavBranches() {
      for (const branch of this.sidebar?.querySelectorAll('[data-nav-branch]') ?? []) {
        this.setNavBranch(branch, this.branchExpanded(branch));
      }
    },

    navQuery() {
      return (this.sidebar?.querySelector('#app-nav-search')?.value ?? '').trim().toLowerCase();
    },

    // The menu filter only hides rows and opens branches; nothing moves or is
    // rebuilt. An empty box shows every row with each branch's own expansion.
    // A live patch restores the server's rows; apply() filters them again.
    applyNavFilter() {
      const nav = this.sidebar?.querySelector('#app-nav');
      if (!nav) return;
      const query = this.navQuery();
      if (query) {
        this.filterNav(nav, query, false);
        return;
      }
      for (const entry of nav.querySelectorAll('.app-nav-item-row, [data-nav-branch]')) {
        entry.hidden = false;
      }
      this.applyNavBranches();
    },

    // Shows the rows under `list` that match, every row under a matching
    // branch (`all`), and every branch on the way to a match, opened. Returns
    // whether anything under `list` matched. The leading row is not a menu
    // entry and always shows.
    filterNav(list, query, all) {
      let matched = false;
      for (const entry of list.children) {
        if (entry.matches('.app-nav-item-row')) {
          const link = entry.querySelector('[data-nav-item]');
          if (!link || link.classList.contains('app-nav-lead')) continue;
          const hit = (link.dataset.navLabel ?? '').toLowerCase().includes(query);
          entry.hidden = !(all || hit);
          matched ||= hit;
        } else if (entry.matches('[data-nav-branch]')) {
          const toggle = entry.querySelector(':scope > .app-nav-parent > [data-nav-toggle]');
          const hit = (toggle?.title ?? '').toLowerCase().includes(query);
          const children = entry.querySelector(':scope > .app-nav-children');
          const below = children ? this.filterNav(children, query, all || hit) : false;
          entry.hidden = !(all || hit || below);
          this.setNavBranch(entry, hit || below || this.branchExpanded(entry));
          matched ||= hit || below;
        }
      }
      return matched;
    },

    setNavBranch(branch, expanded) {
      branch.dataset.navExpanded = expanded ? 'true' : 'false';
      branch.querySelector('[data-nav-toggle]')?.setAttribute('aria-expanded', String(expanded));
      branch.querySelector('.app-nav-children')?.toggleAttribute('hidden', !expanded);
    },

    // The sidebar row a pin control names: its page and label.
    pinFromControl(control) {
      // A record page supplies its own resolved label and same-origin path;
      // the server still validates the destination on /api/pins/toggle.
      if (control.dataset.navPinRecord === 'true') {
        const url = this.normalizePinnedUrl(control.dataset.navPinUrl);
        const label = control.dataset.navPinLabel;
        return url && label ? {label, url, icon: null} : null;
      }
      const item = this.navItem(control.dataset.navPin);
      const url = this.normalizePinnedUrl(item?.getAttribute('href'));
      const label = item?.dataset.navLabel;
      return url && label ? {label, url, icon: null} : null;
    },

    // Pin or unpin a page. The server answers with the account's pins, which
    // replace what is drawn; a failure re-reads them rather than guessing.
    async togglePinnedItem(item) {
      if (this.impersonating || !item || !this.isServedUrl(item.url) || this.pinning) return;
      this.pinning = true;
      try {
        const response = await this.pinRequest('/api/pins/toggle', {
          method: 'POST',
          body: JSON.stringify({label: item.label, url: item.url, icon: item.icon || null}),
        });
        if (!response.ok) throw new Error(`Pin update failed with status ${response.status}`);
        const reply = await response.json();
        this.pinnedEntries = this.acceptServerPins(reply.pins);
        this.renderPinnedItems();
        this.setPinAnnouncement(`${reply.pinned ? 'Pinned' : 'Unpinned'} ${item.label}.`);
      } catch {
        await this.refreshPins();
      } finally {
        this.pinning = false;
      }
    },

    removePinnedItem(key) {
      const item = this.pinnedEntries.find((pinned) => this.pinnedItemKey(pinned) === key);
      return this.togglePinnedItem(item);
    },

    async refreshPins() {
      try {
        const response = await this.pinRequest('/api/pins');
        if (!response.ok) throw new Error(`Pin read failed with status ${response.status}`);
        this.pinnedEntries = this.acceptServerPins((await response.json()).pins);
        this.renderPinnedItems();
      } catch {
        this.setPinAnnouncement('Unable to update pinned pages.');
      }
    },

    async reorderServerPins(pins) {
      const focusedKey = this.pinnedRow(document.activeElement)?.dataset.pinnedItem;
      try {
        const response = await this.pinRequest('/api/pins/reorder', {
          method: 'POST',
          body: JSON.stringify({pins: pins.map((pin) => ({id: pin.pinId}))}),
        });
        if (!response.ok) throw new Error(`Pin reorder failed with status ${response.status}`);
        this.pinnedEntries = this.acceptServerPins((await response.json()).pins);
        this.renderPinnedItems();
        if (focusedKey) this.focusPinnedKey(focusedKey);
      } catch {
        await this.refreshPins();
      }
    },

    focusPinnedKey(key) {
      for (const row of this.pinnedItems?.querySelectorAll('[data-pinned-item]') ?? []) {
        if (row.dataset.pinnedItem === key) {
          row.querySelector('.app-pinned-link')?.focus();
          break;
        }
      }
    },

    navItem(id) {
      const item = document.getElementById(id);
      return this.sidebar?.contains(item) ? item : null;
    },

    // The sidebar row of the pinned page, when the account can see one: its
    // icon is drawn beside the pin.
    navItemForUrl(url) {
      for (const item of this.sidebar?.querySelectorAll('[data-nav-item]') ?? []) {
        if (this.normalizePinnedUrl(item.getAttribute('href')) === url) return item;
      }
      return null;
    },

    resolvePinnedItem(pin) {
      if (!this.isServedUrl(pin.url)) return null;
      return {key: this.pinnedItemKey(pin), pin, item: this.navItemForUrl(pin.url), label: pin.label, url: pin.url};
    },

    pinnedRow(event) {
      const target = event?.target || event;
      if (!target?.closest) return null;
      const row = target.closest('[data-pinned-item]');
      return this.pinnedItems?.contains(row) ? row : null;
    },

    setPinAnnouncement(message) {
      if (this.pinnedAnnouncement) this.pinnedAnnouncement.textContent = message;
    },

    startPinnedDrag(event) {
      const row = this.pinnedRow(event);
      if (!row || !event.dataTransfer) {
        event.preventDefault();
        return;
      }
      this.draggedPinnedKey = row.dataset.pinnedItem;
      event.dataTransfer.effectAllowed = 'move';
      event.dataTransfer.setData('text/plain', this.draggedPinnedKey);
      row.dataset.pinnedDragging = 'true';
    },

    overPinnedDrag(event) {
      const row = this.pinnedRow(event);
      if (!row || !this.draggedPinnedKey || !event.dataTransfer) return;
      event.preventDefault();
      event.dataTransfer.dropEffect = 'move';
      if (row.dataset.pinnedItem === this.draggedPinnedKey) return;
      this.setPinnedDropTarget(row.dataset.pinnedItem);
    },

    dropPinnedDrag(event) {
      const row = this.pinnedRow(event);
      if (!row || !this.draggedPinnedKey) return;
      event.preventDefault();
      const targetKey = row.dataset.pinnedItem;
      const draggedKey = this.draggedPinnedKey;
      if (targetKey !== draggedKey) {
        const dragged = this.pinnedEntries.find((item) => this.pinnedItemKey(item) === draggedKey);
        const reordered = this.pinnedEntries.filter((item) => this.pinnedItemKey(item) !== draggedKey);
        const targetIndex = reordered.findIndex((item) => this.pinnedItemKey(item) === targetKey);
        if (dragged && targetIndex >= 0) {
          reordered.splice(targetIndex, 0, dragged);
          this.pinnedEntries = reordered;
          this.reorderServerPins(reordered);
        }
      }
      this.endPinnedDrag();
      this.renderPinnedItems();
    },

    endPinnedDrag() {
      this.draggedPinnedKey = null;
      this.dropPinnedKey = null;
      for (const row of this.pinnedItems?.querySelectorAll('[data-pinned-item]') ?? []) {
        delete row.dataset.pinnedDragging;
        delete row.dataset.pinnedDropTarget;
      }
    },

    setPinnedDropTarget(id) {
      if (this.dropPinnedKey === id) return;
      this.dropPinnedKey = id;
      for (const row of this.pinnedItems?.querySelectorAll('[data-pinned-item]') ?? []) {
        if (row.dataset.pinnedItem === id) row.dataset.pinnedDropTarget = 'true';
        else delete row.dataset.pinnedDropTarget;
      }
    },

    // Pins to a page this deployment no longer serves are not drawn, but stay
    // stored, so a restored page brings them back.
    renderPinnedItems() {
      if (!this.pinned || !this.pinnedItems) return;
      const items = this.pinnedEntries.map((pin) => this.resolvePinnedItem(pin)).filter(Boolean);
      // Rebuilding is a child-list mutation that ui.js reports as `updated`,
      // which calls back here: rebuild only when the drawn pins change.
      const drawn = JSON.stringify([this.impersonating, items.map(({key, label, url}) => [key, label, url])]);
      if (drawn !== this.drawnPins || this.pinnedItems.childElementCount !== items.length) {
        this.drawnPins = drawn;
        this.rebuildPinnedItems(items);
      }
      for (const pin of this.root.querySelectorAll('[data-nav-pin]')) {
        const url = this.pinFromControl(pin)?.url;
        const pinned = this.pinnedEntries.some((item) => url && this.pinnedUrl(item) === url);
        pin.disabled = this.impersonating;
        pin.setAttribute('aria-disabled', String(this.impersonating));
        pin.dataset.pinned = String(pinned);
        pin.setAttribute('aria-pressed', String(pinned));
        pin.title = `${pinned ? 'Unpin' : 'Pin'} ${pin.getAttribute('aria-label')?.replace(/^(Pin|Unpin) /, '')}`;
      }
      this.pinned.hidden = items.length === 0;
      if (items.length === 0) this.setPinnedMenu(false);
    },

    rebuildPinnedItems(items) {
      this.pinnedItems.replaceChildren();
      for (const {key, item, label: pinLabel, url} of items) {
        const row = document.createElement('div');
        row.className = 'app-pinned-row group flex min-w-0 items-center rounded-sm lg:shrink-0';
        row.dataset.pinnedItem = key;
        row.draggable = !this.impersonating;

        const link = document.createElement('a');
        link.href = url;
        link.className = 'app-pinned-link flex min-w-0 flex-1 items-center rounded-sm px-1 py-0 text-sm font-normal text-link transition hover:bg-surface-muted hover:text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-strong';
        const icon = item?.querySelector('.app-nav-icon')?.cloneNode(true);
        if (icon) link.append(icon);
        const label = document.createElement('span');
        label.className = ['app-nav-label app-pinned-label min-w-0 truncate', item && 'ml-1.5'].filter(Boolean).join(' ');
        label.textContent = pinLabel;
        link.append(label);

        const unpin = document.createElement('button');
        unpin.type = 'button';
        unpin.dataset.navUnpin = key;
        unpin.disabled = this.impersonating;
        unpin.title = `Unpin ${pinLabel}`;
        unpin.setAttribute('aria-label', `Unpin ${pinLabel}`);
        unpin.className = 'app-pinned-unpin grid size-4 shrink-0 place-items-center rounded-sm text-muted transition hover:bg-surface-muted hover:text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-strong lg:hidden lg:group-focus-within:grid lg:group-hover:grid';
        const pinIcon = this.sidebar?.querySelector('[data-nav-pin] svg')?.cloneNode(true);
        if (pinIcon) unpin.append(pinIcon);

        const tile = document.createElement('a');
        tile.href = url;
        tile.id = `pinned-tile-${key.replace(/[^a-z0-9]+/gi, '-')}`;
        tile.dataset.navTile = url;
        tile.title = `Open ${pinLabel} in a tile`;
        tile.setAttribute('aria-label', `Open ${pinLabel} in a tile`);
        tile.className = unpin.className.replace('app-pinned-unpin', 'app-pinned-tile');
        const tileIcon = document.createElement('span');
        tileIcon.className = 'hero-squares-plus size-3.5';
        tileIcon.setAttribute('aria-hidden', 'true');
        tile.append(tileIcon);

        row.append(link, tile, unpin);
        this.pinnedItems.append(row);
      }
    },

    focusable() {
      return Array.from(this.sidebar?.querySelectorAll(FOCUSABLE) ?? []).filter(
        (node) => !node.hasAttribute('disabled') && node.offsetParent !== null);
    },

    focusSidebar() {
      const nodes = this.focusable();
      (nodes[0] || this.sidebar)?.focus?.();
    },

    apply() {
      const desktop = this.desktop();
      const open = desktop ? !this.hidden : this.drawerOpen;
      this.root.dataset.sidebarMode = desktop ? 'desktop' : 'mobile';
      this.root.dataset.sidebarHidden = desktop && this.hidden ? 'true' : 'false';
      this.root.dataset.sidebarOpen = open ? 'true' : 'false';
      if (this.toggle) {
        const label = open ? 'Hide menu' : 'Show menu';
        this.toggle.setAttribute('aria-expanded', open ? 'true' : 'false');
        this.toggle.setAttribute('aria-label', label);
        this.toggle.setAttribute('title', label);
      }
      const drawerOpen = !desktop && this.drawerOpen;
      const hideDrawer = !desktop && !this.drawerOpen;
      if (this.sidebar) {
        this.sidebar.toggleAttribute('inert', hideDrawer);
        this.sidebar.setAttribute('aria-hidden', hideDrawer ? 'true' : 'false');
        this.sidebar.setAttribute('aria-modal', drawerOpen ? 'true' : 'false');
        this.sidebar.setAttribute('role', drawerOpen ? 'dialog' : 'navigation');
        this.sidebar.style.width = desktop ? `${this.width}px` : '';
      }
      this.drag?.setAttribute('aria-valuenow', String(this.width));
      for (const region of [this.content, this.statusbar, this.topbarMain]) {
        if (!region) continue;
        region.toggleAttribute('inert', drawerOpen);
        region.setAttribute('aria-hidden', drawerOpen ? 'true' : 'false');
      }
      if (this.backdrop) {
        this.backdrop.setAttribute('aria-hidden', drawerOpen ? 'false' : 'true');
        this.backdrop.style.pointerEvents = drawerOpen ? 'auto' : 'none';
      }
      this.applyNavFilter();
      this.renderPinnedItems();
    },
  };

  // A page inside a workspace tile has no chrome; it follows the same theme.
  const FramedDisplay = {
    mounted() { projectTheme(this.el.dataset.themeChoice); },
    updated() { this.mounted(); },
  };

  window.BilibiUI.registerHook('AppShell', AppShell);
  window.BilibiUI.registerHook('FramedDisplay', FramedDisplay);
})();
