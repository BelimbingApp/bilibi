// The browser half of Base Tiling's workspace (`#workspace`, data-hook="Tiling"),
// adapted from Bilimbi apps/web/assets/js/tiling.js. The server owns the tree,
// the focused tile and monocle, and renders one element per tile positioned by
// `data-place`. This hook owns only what the server cannot reach:
//
//   * the keyboard bridge into every frame. A keydown inside a frame never
//     reaches the parent document, so the same listener is attached to each
//     frame's document on load. The frames are same-origin, so no message
//     protocol is needed;
//   * the `Ctrl+.` tiling mode, a Hyprland submap: single-key commands with
//     Escape to leave, so nothing collides with what the operating system or
//     the browser reserves. The mode names itself in the shell's `#app-mode`
//     status region. Each tile offers move, flip and close controls;
//     the mode provides the other operations, and each key is
//     documented in apps/base/tiling/README.md, and a key added here is added
//     there;
//   * moving real focus into the tile the server has focused after a
//     keyboard move, so the ring and the caret agree;
//   * dragging a split handle. A pointer over a frame is the frame's, so
//     frames stop receiving pointer events for the length of a drag; the
//     handle follows the pointer locally and the ratio is sent once on
//     release. A focused handle moves one step per arrow key on its own axis;
//   * dragging a tile by its grip onto another tile. The tile under the
//     pointer is marked as the drop target, and the swap is sent once on
//     release; a press that never travels does nothing. The mode's `s` is the
//     pointer-free way to swap;
//   * focusing a tile when its own chrome is pressed. A press inside the page
//     reaches the frame's document instead, which reports it the same way;
//   * reporting each frame's address and title after it loads or its live
//     page changes address (live.js raises `bilibi:location` in the frame),
//     and the workspace area's size, which decides a split's direction. A
//     report only records where a tile already is, so it asks the transport
//     to replace the workspace address rather than push a new entry;
//   * placing every tile and handle. The server sends each position as
//     `data-place`, and this hook writes it through the CSSOM after every
//     patch, because the Content-Security-Policy allows no `style` attribute.
//     The patcher drops what the server did not render (`style`,
//     `data-placed`, `data-mode`, a drag's marks), so they are written again
//     after every patch, and only where they differ;
//   * both ends of the follow channel (base/ui `src/workspace.rs`). The frames
//     are same-origin and the channel is `postMessage`: a message counts only
//     from this origin and, on this side, from one of the workspace's own
//     frames, on the frame's side from the window that framed it. The host
//     tells each frame the kinds some tile follows (`data-follows`); a frame
//     tells the host the record a page announced or a record link selected,
//     and the server answers with `tile_navigate` for each following tile,
//     whose path is checked to be this origin's before the frame goes there.
//
// Focus follows a click, never the pointer.
(() => {
  const LEADER_KEY = '.';
  const DIRECTIONS = {
    ArrowLeft: 'left',
    ArrowRight: 'right',
    ArrowUp: 'up',
    ArrowDown: 'down',
    h: 'left',
    j: 'down',
    k: 'up',
    l: 'right',
  };
  const MOVES = {H: 'left', J: 'down', K: 'up', L: 'right'};
  const ARROWS = {ArrowLeft: 'left', ArrowRight: 'right', ArrowUp: 'up', ArrowDown: 'down'};
  const MODE_LABELS = {tiling: 'Tiling', resize: 'Resize'};
  const PLACE_KEYS = ['left', 'top', 'width', 'height'];
  const MIN_RATIO = 0.1;
  const MAX_RATIO = 0.9;
  // Pointer travel before a press on a grip becomes a drag, in CSS pixels.
  const DRAG_THRESHOLD = 6;
  // Every page's document title is the product name until pages name
  // themselves; a tile then takes its page's heading.
  const PRODUCT = 'Bilibi';
  // The follow channel's two messages, and the shape of what they carry; the
  // server checks a fact again (base/ui `src/workspace.rs`).
  const FOLLOWS = 'bilibi:workspace:follows';
  const FACT = 'bilibi:workspace:fact';
  const KIND = /^[a-z][a-z0-9_]*\/[a-z][a-z0-9_]*$/;
  const isKind = (kind) => typeof kind === 'string' && KIND.test(kind);
  const isRecordId = (id) =>
    typeof id === 'string' && id !== '' && id.length <= 64 && id !== '.' && id !== '..' && !/[/?#\\\s\x00-\x1f\x7f]/.test(id);
  // A path the server names for a frame: a page of this origin, nothing else.
  function samePath(path) {
    if (typeof path !== 'string' || !path.startsWith('/') || path.startsWith('//') || path.includes('\\')) return null;
    try {
      const url = new URL(path, location.origin);
      return url.origin === location.origin ? url.pathname + url.search : null;
    } catch {
      return null;
    }
  }

  function parsePlace(text) {
    const place = {};
    for (const part of (text || '').split(';')) {
      const [name, value] = part.split(':').map((s) => s && s.trim());
      if (PLACE_KEYS.includes(name) && /^-?\d+(\.\d+)?%$/.test(value || '')) place[name] = value;
    }
    return place;
  }

  const Tiling = {
    mounted() {
      this.mode = 'off';
      this.lastSide = 'right';
      this.pendingFocus = false;
      this.appliedFocus = null;
      this.drag = null;
      this.tileDrag = null;
      this.bound = new WeakSet();
      this.reported = new WeakMap();
      this.container = this.el.querySelector('#workspace-tiles');
      this.announcement = this.el.querySelector('#workspace-announcement');

      this.onKey = (event) => this.handleKey(event);
      this.onHandleKey = (event) => this.handleHandleKey(event);
      this.onPointerDown = (event) => this.startDrag(event);
      this.onPointerMove = (event) => this.moveDrag(event);
      this.onPointerUp = () => this.endDrag();
      this.onTilePointerDown = (event) => this.pressTile(event);
      this.onTilePointerMove = (event) => this.dragTile(event);
      this.onTilePointerUp = () => this.dropTile();
      this.onTileClick = (event) => this.swallowClick(event);
      this.onViewport = () => this.scheduleViewport();
      this.onMessage = (event) => this.receive(event);
      this.onNavigate = (event) => this.navigateFrame(event.detail || {});
      this.follows = null;

      // Capture, so the mode sees a key before the shell's own Escape handling
      // and before any page-level binding.
      window.addEventListener('keydown', this.onKey, true);
      this.el.addEventListener('keydown', this.onHandleKey);
      this.el.addEventListener('pointerdown', this.onPointerDown);
      this.el.addEventListener('pointerdown', this.onTilePointerDown);
      this.el.addEventListener('click', this.onTileClick, true);
      window.addEventListener('resize', this.onViewport);
      window.addEventListener('message', this.onMessage);
      window.addEventListener('bilibi:tile_navigate', this.onNavigate);

      this.place();
      this.bindFrames();
      this.publishFollows();
      this.reportViewport();
    },

    updated() {
      // A patch writes this element's attributes back to what the server
      // rendered; the mode and a drag in progress are the hook's.
      if (this.el.dataset.mode !== this.mode) this.el.dataset.mode = this.mode;
      if (this.drag || this.tileDrag?.active) this.freezeFrames(true);
      if (this.tileDrag?.active) {
        this.el.dataset.tileDrag = this.tileDrag.id;
        this.tileElement(this.tileDrag.id)?.setAttribute('data-dragging', 'true');
        this.markDropTarget(this.tileDrag.over);
      }
      this.place();
      this.bindFrames();
      this.nameFrames();
      this.publishFollows();
      this.applyFocus();
    },

    destroyed() {
      this.setMode('off');
      window.removeEventListener('keydown', this.onKey, true);
      this.el.removeEventListener('keydown', this.onHandleKey);
      this.el.removeEventListener('pointerdown', this.onPointerDown);
      this.el.removeEventListener('pointerdown', this.onTilePointerDown);
      this.el.removeEventListener('click', this.onTileClick, true);
      window.removeEventListener('resize', this.onViewport);
      window.removeEventListener('message', this.onMessage);
      window.removeEventListener('bilibi:tile_navigate', this.onNavigate);
      window.removeEventListener('pointermove', this.onPointerMove);
      window.removeEventListener('pointerup', this.onPointerUp);
      window.removeEventListener('pointermove', this.onTilePointerMove);
      window.removeEventListener('pointerup', this.onTilePointerUp);
      if (this.viewportFrame) cancelAnimationFrame(this.viewportFrame);
    },

    // `data-place` is `left: 0%; top: 50%; width: 50%; height: 50%`, the four
    // properties the server computed. A property a handle leaves out (its
    // thickness is a class) is cleared, so a split that flipped direction does
    // not keep the old axis's length. A handle being dragged keeps the place
    // the pointer gave it until the release is answered.
    place() {
      for (const el of this.el.querySelectorAll('[data-place]')) {
        if (this.drag && el === this.drag.handle) continue;
        const place = parsePlace(el.dataset.place);
        for (const name of PLACE_KEYS) {
          const value = place[name] || '';
          if (el.style.getPropertyValue(name) !== value) el.style.setProperty(name, value);
        }
      }
      if (this.el.dataset.placed !== 'true') this.el.dataset.placed = 'true';
    },

    // ---------------------------------------------------------------
    // Frames
    // ---------------------------------------------------------------

    frames() {
      return [...this.el.querySelectorAll('iframe[data-tile-frame]')];
    },

    bindFrames() {
      for (const frame of this.frames()) {
        if (this.bound.has(frame)) continue;
        this.bound.add(frame);
        frame.addEventListener('load', () => this.attachFrame(frame));
        if (this.frameReady(frame)) this.attachFrame(frame);
      }
    },

    // A frame's `title` is its accessible name, and with no bar above the tile
    // it is the only one. The server renders the frame once, before the page
    // has reported its title, so the name is kept in step from `data-title`.
    nameFrames() {
      for (const frame of this.frames()) {
        const title = this.tileElement(frame.dataset.tileFrame)?.dataset.title;
        if (title && frame.title !== title) frame.title = title;
      }
    },

    frameReady(frame) {
      try {
        const doc = frame.contentDocument;
        return Boolean(doc && doc.readyState === 'complete' && doc.location?.href !== 'about:blank');
      } catch {
        return false;
      }
    },

    // Every load is a new document, so the listeners go on again; the old
    // document took its own away.
    attachFrame(frame) {
      let win;
      let doc;
      try {
        win = frame.contentWindow;
        doc = frame.contentDocument;
      } catch {
        return;
      }
      if (!win || !doc || doc.location?.href === 'about:blank') return;

      const id = frame.dataset.tileFrame;
      doc.addEventListener('keydown', this.onKey, true);
      doc.addEventListener('focusin', () => this.focusTile(id));
      doc.addEventListener('pointerdown', () => this.focusTile(id));
      win.addEventListener('bilibi:location', () => this.report(frame));
      this.tellFollows(win);
      this.report(frame);
    },

    // A page names itself by its document title or, while that is only the
    // product's name, by its heading.
    frameTitle(doc) {
      const title = (doc.title || '').trim();
      if (title && title !== PRODUCT) return title;
      return doc.querySelector('#app-content h1')?.textContent.replace(/\s+/g, ' ').trim() || '';
    },

    report(frame) {
      let path;
      let title;
      try {
        const win = frame.contentWindow;
        const doc = frame.contentDocument;
        if (!win?.location || !doc) return;
        path = win.location.pathname + win.location.search;
        title = this.frameTitle(doc);
      } catch {
        return;
      }
      const key = `${path}\n${title}`;
      if (this.reported.get(frame) === key) return;
      this.reported.set(frame, key);
      this.emit('tile_navigated', {id: frame.dataset.tileFrame, path, title, history: 'replace'});
    },

    // ---------------------------------------------------------------
    // The follow channel
    // ---------------------------------------------------------------

    followedKinds() {
      return (this.el.dataset.follows || '').split(' ').filter(isKind);
    },

    // Every frame learns the followed kinds when they change, and each new
    // document learns them once it has loaded.
    publishFollows() {
      const kinds = this.followedKinds().join(' ');
      if (kinds === this.follows) return;
      this.follows = kinds;
      for (const frame of this.frames()) {
        try {
          this.tellFollows(frame.contentWindow);
        } catch {
          /* not reachable */
        }
      }
    },

    tellFollows(win) {
      win?.postMessage({type: FOLLOWS, kinds: this.followedKinds()}, location.origin);
    },

    // A fact from a page in one of this workspace's own tiles; anything else
    // that reaches this window is ignored.
    receive(event) {
      if (event.origin !== location.origin) return;
      const data = event.data;
      if (!data || data.type !== FACT || !isKind(data.kind) || !isRecordId(data.id)) return;
      if (!this.frames().some((frame) => frame.contentWindow === event.source)) return;
      this.emit('record_selected', {kind: data.kind, id: data.id, history: 'replace'});
    },

    // A following tile goes to the record another tile selected. The frame
    // replaces its page, so Back still leaves the workspace.
    navigateFrame({id, path}) {
      const target = samePath(path);
      const frame = target && this.el.querySelector(`iframe[data-tile-frame="${CSS.escape(String(id))}"]`);
      if (!frame) return;
      try {
        frame.contentWindow.location.replace(target);
      } catch {
        /* not reachable */
      }
    },

    focusTile(id) {
      if (!id || this.el.dataset.focused === id) return;
      this.emit('focus_tile', {id});
    },

    // After a keyboard move the server has a new focused tile; put real focus
    // there so the next key lands in it.
    applyFocus() {
      const focused = this.el.dataset.focused;
      if (!this.pendingFocus || !focused || focused === this.appliedFocus) return;

      this.pendingFocus = false;
      this.appliedFocus = focused;

      // A tile with no frame shows a refusal; focus its move control.
      const frame = this.el.querySelector(`iframe[data-tile-frame="${focused}"]`);
      if (frame) {
        frame.focus();
        try { frame.contentWindow?.focus?.(); } catch { /* not reachable */ }
      } else {
        this.tileElement(focused)?.querySelector('button[data-role="tile-grip"]')?.focus();
      }
    },

    // ---------------------------------------------------------------
    // Keyboard mode
    // ---------------------------------------------------------------

    handleKey(event) {
      if (this.leader(event)) {
        event.preventDefault();
        event.stopPropagation();
        this.setMode(this.mode === 'off' ? 'tiling' : 'off');
        return;
      }

      if (this.mode === 'off') return;

      if (event.key === 'Escape') {
        // An open menu or panel takes Escape first, as everywhere in the shell.
        if (this.openDisclosure()) return;
        event.preventDefault();
        event.stopPropagation();
        this.setMode(this.mode === 'resize' ? 'tiling' : 'off');
        return;
      }

      if (event.ctrlKey || event.altKey || event.metaKey) return;

      const handled = this.mode === 'resize' ? this.resizeKey(event) : this.tilingKey(event);

      if (handled) {
        event.preventDefault();
        event.stopPropagation();
      }
    },

    leader(event) {
      return event.key === LEADER_KEY && event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey;
    },

    tilingKey(event) {
      const focused = this.el.dataset.focused || null;
      const side = event.shiftKey ? null : DIRECTIONS[event.key];

      if (side) {
        this.lastSide = side;
        this.expectFocus();
        this.emit('focus_direction', {side});
        return true;
      }

      const move = MOVES[event.key];
      if (move) {
        if (focused) {
          this.expectFocus();
          this.emit('move_tile', {id: focused, side: move});
        }
        return true;
      }

      switch (event.key) {
        case 's':
          if (focused) this.emit('swap_tile', {id: focused, side: this.lastSide});
          return true;
        case 'r':
          this.setMode('resize');
          return true;
        case 'f':
          if (focused) this.emit('toggle_monocle', {id: focused});
          return true;
        case 't':
          if (focused) this.emit('toggle_split', {id: focused});
          return true;
        case 'm':
          if (focused) this.emit('make_master', {id: focused});
          return true;
        case 'o':
          if (focused) this.emit('open_alone', {id: focused});
          return true;
        // The server renders `data-follow="on"` on a tile that follows.
        // Asking a page with no record to follow is the server's to answer.
        case 'w':
          if (focused) {
            const following = this.tileElement(focused)?.dataset.follow === 'on';
            this.emit(following ? 'unfollow_tile' : 'follow_tile', {id: focused});
          }
          return true;
        case 'n':
          this.openPicker();
          return true;
        case 'q':
          if (focused) {
            this.expectFocus();
            this.emit('close_tile', {id: focused});
          }
          return true;
        default:
          return false;
      }
    },

    resizeKey(event) {
      const side = DIRECTIONS[event.key];
      if (!side || !this.el.dataset.focused) return false;
      this.emit('resize_step', {side});
      return true;
    },

    // The picker is a dialog the person types into, so the mode ends first.
    openPicker() {
      this.setMode('off');
      this.emit('open_picker', {});
    },

    expectFocus() {
      this.pendingFocus = true;
      this.appliedFocus = null;
    },

    setMode(mode) {
      this.mode = mode;
      this.el.dataset.mode = mode;

      const target = document.getElementById('app-mode');
      if (!target) return;

      if (mode === 'off') {
        target.hidden = true;
        target.textContent = '';
      } else {
        target.textContent = MODE_LABELS[mode];
        target.hidden = false;
      }
    },

    // An open menu or panel, such as the account and display panels. The
    // sidebar toggle and an expanded sidebar branch also say `aria-expanded`,
    // and Escape has never collapsed either on a wide screen, so they do not
    // count.
    openDisclosure() {
      return Boolean(
        document.querySelector('#app-shell [aria-expanded="true"]:not([data-nav-toggle]):not(#app-sidebar-toggle)')
      );
    },

    announce(message) {
      if (this.announcement) this.announcement.textContent = message;
    },

    // ---------------------------------------------------------------
    // Split handles
    // ---------------------------------------------------------------

    handleHandleKey(event) {
      const handle = event.target.closest?.('[data-split]');
      const side = ARROWS[event.key];
      if (!handle || !side || !this.el.contains(handle)) return;
      const horizontal = handle.dataset.direction === 'h';
      if (horizontal !== (side === 'left' || side === 'right')) return;
      event.preventDefault();
      this.emit('nudge_split', {id: handle.dataset.split, side});
    },

    startDrag(event) {
      const handle = event.target.closest?.('[data-split]');
      if (!handle || event.button !== 0 || !this.container) return;

      event.preventDefault();
      const [x, y, w, h] = (handle.dataset.rect || '0 0 1 1').split(' ').map(Number);

      this.drag = {
        handle,
        id: handle.dataset.split,
        direction: handle.dataset.direction,
        area: this.container.getBoundingClientRect(),
        split: {x, y, w, h},
        ratio: null,
      };

      this.freezeFrames(true);
      document.documentElement.style.cursor = this.drag.direction === 'h' ? 'col-resize' : 'row-resize';
      document.documentElement.style.userSelect = 'none';
      window.addEventListener('pointermove', this.onPointerMove);
      window.addEventListener('pointerup', this.onPointerUp);
    },

    moveDrag(event) {
      if (!this.drag) return;
      const {handle, direction, area, split} = this.drag;

      let ratio;
      if (direction === 'h') {
        const fraction = (event.clientX - area.left) / area.width;
        ratio = (fraction - split.x) / split.w;
      } else {
        const fraction = (event.clientY - area.top) / area.height;
        ratio = (fraction - split.y) / split.h;
      }

      ratio = Math.min(MAX_RATIO, Math.max(MIN_RATIO, ratio));
      this.drag.ratio = ratio;

      if (direction === 'h') {
        handle.style.left = `${(split.x + split.w * ratio) * 100}%`;
      } else {
        handle.style.top = `${(split.y + split.h * ratio) * 100}%`;
      }
    },

    endDrag() {
      if (!this.drag) return;
      const {id, ratio} = this.drag;
      this.drag = null;

      this.freezeFrames(false);
      document.documentElement.style.cursor = '';
      document.documentElement.style.userSelect = '';
      window.removeEventListener('pointermove', this.onPointerMove);
      window.removeEventListener('pointerup', this.onPointerUp);

      if (ratio !== null) this.emit('resize_split', {id, ratio: Number(ratio.toFixed(3))});
      else this.place();
    },

    // While a pointer drags, it must stay the workspace's even over a frame.
    freezeFrames(frozen) {
      for (const frame of this.frames()) {
        const value = frozen ? 'none' : '';
        if (frame.style.pointerEvents !== value) frame.style.pointerEvents = value;
      }
    },

    // ---------------------------------------------------------------
    // Dragging a tile onto another
    // ---------------------------------------------------------------

    pressTile(event) {
      const tile = event.target.closest?.('[data-tile]');
      if (!tile || event.button !== 0) return;

      this.focusTile(tile.dataset.tile);
      if (!event.target.closest('[data-role="tile-grip"]')) return;

      event.preventDefault();
      this.tileDrag = {id: tile.dataset.tile, x: event.clientX, y: event.clientY, active: false, over: null};
      window.addEventListener('pointermove', this.onTilePointerMove);
      window.addEventListener('pointerup', this.onTilePointerUp);
    },

    dragTile(event) {
      const drag = this.tileDrag;
      if (!drag) return;

      if (!drag.active) {
        if (Math.hypot(event.clientX - drag.x, event.clientY - drag.y) < DRAG_THRESHOLD) return;
        drag.active = true;
        this.el.dataset.tileDrag = drag.id;
        this.tileElement(drag.id)?.setAttribute('data-dragging', 'true');
        this.freezeFrames(true);
        document.documentElement.style.cursor = 'grabbing';
        document.documentElement.style.userSelect = 'none';
      }

      const over = document
        .elementFromPoint(event.clientX, event.clientY)
        ?.closest?.('[data-tile]')?.dataset.tile;
      this.markDropTarget(over && over !== drag.id ? over : null);
    },

    dropTile() {
      const drag = this.tileDrag;
      if (!drag) return;
      this.tileDrag = null;
      window.removeEventListener('pointermove', this.onTilePointerMove);
      window.removeEventListener('pointerup', this.onTilePointerUp);
      if (!drag.active) return;

      // The click that follows the release would press whatever control the
      // pointer ended over, on a tile that is not the one that was pressed. A
      // release outside the workspace sends no click here, so the flag clears
      // itself once the click had its chance.
      this.swallowNextClick = true;
      setTimeout(() => (this.swallowNextClick = false), 0);
      delete this.el.dataset.tileDrag;
      this.tileElement(drag.id)?.removeAttribute('data-dragging');
      this.markDropTarget(null);
      this.freezeFrames(false);
      document.documentElement.style.cursor = '';
      document.documentElement.style.userSelect = '';

      if (drag.over) {
        this.emit('swap_tile', {id: drag.id, with: drag.over});
        this.announce(`Swapped ${this.tileTitle(drag.id)} and ${this.tileTitle(drag.over)}.`);
      }
    },

    markDropTarget(id) {
      if (this.tileDrag) this.tileDrag.over = id;
      for (const tile of this.el.querySelectorAll('[data-tile]')) {
        if (tile.dataset.tile === id) tile.setAttribute('data-drop-target', 'true');
        else tile.removeAttribute('data-drop-target');
      }
    },

    swallowClick(event) {
      if (this.swallowNextClick) {
        this.swallowNextClick = false;
        event.preventDefault();
        event.stopPropagation();
        return;
      }
      const grip = event.target.closest?.('[data-role="tile-grip"]');
      if (grip) {
        this.focusTile(grip.closest('[data-tile]')?.dataset.tile);
        this.setMode('tiling');
        this.announce('Tiling mode. Use H, J, K or L with Shift to move the tile.');
      }
    },

    tileElement(id) {
      return this.el.querySelector(`[data-tile="${id}"]`);
    },

    tileTitle(id) {
      return this.tileElement(id)?.dataset.title?.trim() || id;
    },

    // ---------------------------------------------------------------
    // Viewport
    // ---------------------------------------------------------------

    scheduleViewport() {
      if (this.viewportFrame) return;
      this.viewportFrame = requestAnimationFrame(() => {
        this.viewportFrame = null;
        this.reportViewport();
      });
    },

    reportViewport() {
      const area = this.container || this.el;
      const w = Math.round(area.clientWidth);
      const h = Math.round(area.clientHeight);
      if (w > 0 && h > 0 && (w !== this.lastW || h !== this.lastH)) {
        this.lastW = w;
        this.lastH = h;
        this.emit('viewport', {w, h});
      }
    },
  };

  window.BilibiUI.registerHook('Tiling', Tiling);

  // ---- The frame side of the follow channel ----------------------------------
  // A page in a tile learns from the workspace which kinds some tile follows.
  // A record link in a tile is base/ui `lists::record_link`'s
  // `workspace:select` button (Bilimbi's `<.record_link>`); its click is
  // answered here and never reaches the server. When its kind is followed it
  // announces the record and the page stays where it is; otherwise the frame
  // opens the record's page, as the link does alone, replacing its address so
  // Back still leaves the workspace. A page that shows a record
  // (`data-workspace-record`) announces it once per address, when the page is
  // ready in the tile.
  if (window.parent === window) return;
  let follows = [];
  let announced = null;
  const post = (message) => {
    try {
      window.parent.postMessage(message, location.origin);
    } catch {
      /* the workspace is gone */
    }
  };
  const announce = () => {
    const record = document.querySelector('[data-workspace-record]');
    const kind = record?.dataset.recordKind;
    const id = record?.dataset.recordId;
    const key = record ? `${location.pathname}${location.search}\n${kind}\n${id}` : null;
    if (key === announced) return;
    announced = key;
    if (record && follows.includes(kind) && isRecordId(id)) post({type: FACT, kind, id});
  };
  window.addEventListener('message', (event) => {
    if (event.source !== window.parent || event.origin !== location.origin) return;
    const data = event.data;
    if (!data || data.type !== FOLLOWS || !Array.isArray(data.kinds)) return;
    follows = data.kinds.filter(isKind);
    announce();
  });
  window.addEventListener('bilibi:location', announce);
  // Capture, so the choice is made before live.js would send the button's
  // event or turn a plain link into the frame's own navigation.
  document.addEventListener('click', (event) => {
    if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    const control = event.target instanceof Element && event.target.closest('[data-record-select]');
    if (!control) return;
    const {kind, id, path} = control.dataset;
    event.preventDefault();
    event.stopImmediatePropagation();
    if (follows.includes(kind) && isRecordId(id)) {
      post({type: FACT, kind, id});
      return;
    }
    const target = samePath(path);
    if (target) location.replace(target);
  }, true);
})()
