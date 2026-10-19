// Read-first editors, adapted from Bilimbi InlineEdit/InlineLongText and
// inline_commit. Every commit travels through the queued transport; only its
// own reply ends Saving. Background patches never acknowledge a write.
(() => {
  'use strict';
  const hook = {
    mounted() {
      this.active = false;
      this.draft = null;
      this.pending = new Set();
      this.sequence = 0;
      this.long = this.el.dataset.hook === 'InlineLongText';
      this.choice = this.el.dataset.hook === 'InlineChoice';
      this.abort = new AbortController();
      const listen = (node, type, callback) => node.addEventListener(type, callback, {signal: this.abort.signal});
      listen(this.el, 'click', (event) => {
        if (event.target.closest('[data-role="trigger"]')) this.start();
      });
      listen(this.el, 'keydown', (event) => {
        if (event.isComposing || event.defaultPrevented) return;
        if (event.target.closest('[data-role="trigger"]')) {
          if (['Enter', ' ', 'F2'].includes(event.key)) {
            event.preventDefault();
            this.start(undefined, event.key === 'F2');
          } else if (!this.choice && event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
            event.preventDefault();
            this.start(event.key);
          }
        } else if (this.active && event.key === 'Escape') {
          event.preventDefault();
          event.stopPropagation();
          this.cancel(true);
        } else if (this.active && !this.long && event.key === 'Enter') {
          event.preventDefault();
          this.commit(true);
        }
      });
      // A combobox consumes Escape itself; capture still cancels the shell.
      listen(this.el, 'keyup', (event) => {
        if (this.active && event.key === 'Escape' && !event.isComposing) this.cancel(true);
      });
      listen(this.el, 'focusout', () => {
        queueMicrotask(() => {
          if (!this.active || this.editor()?.contains(document.activeElement)) return;
          if (this.choice) this.cancel(false);
          else this.commit(false);
        });
      });
      listen(this.el, 'change', (event) => {
        if (this.choice && this.active && event.target.name === this.el.dataset.field) this.commit(true);
      });
      this.updated();
      if (this.el.dataset.initialEdit === 'true') this.start(undefined, false, this.el.dataset.autofocus !== 'false');
    },
    destroyed() { this.abort.abort(); },
    editor() { return this.el.querySelector('[data-role="editor"]'); },
    control() {
      return [...(this.editor()?.querySelectorAll('[name]') || [])].find((input) => input.name === this.el.dataset.field);
    },
    trigger() { return this.el.querySelector('[data-role="trigger"]'); },
    updated() {
      // Permission loss removes the editor on the server and discards its draft.
      if (this.el.dataset.editable !== 'true') {
        this.active = false;
        this.draft = null;
      }
      // The owner's hint, then the outcome, as the template renders them.
      const describedBy = [
        this.el.querySelector('[data-role="hint"]') && `${this.el.id}-hint`,
        this.el.dataset.status !== 'none' && `${this.el.id}-status`,
      ].filter(Boolean).join(' ');
      const trigger = this.trigger();
      if (trigger) {
        trigger.classList.toggle(this.long || this.choice ? 'hidden' : 'invisible', this.active);
        trigger.classList.toggle('pointer-events-none', this.active);
        if (describedBy) trigger.setAttribute('aria-describedby', describedBy);
        else trigger.removeAttribute('aria-describedby');
      }
      this.editor()?.classList.toggle('hidden', !this.active);
      const input = this.control();
      if (input) {
        input.setAttribute('aria-invalid', String(this.el.dataset.status === 'error'));
        if (describedBy) input.setAttribute('aria-describedby', describedBy);
        else input.removeAttribute('aria-describedby');
      }
      // Only committed server data paints the fact, even on failure.
      const text = this.el.querySelector('[data-role="text"]');
      if (text) {
        const value = this.el.dataset.value;
        const placeholder = this.el.querySelector('input[data-role="input"]')?.placeholder || '—';
        // The source HEEx wraps a long value in line breaks inside its
        // pre-wrap span; a fact shows the value where its field's text sits.
        const displayed = value || placeholder;
        if (text.textContent !== displayed) text.textContent = displayed;
        text.classList.toggle('text-ink-muted', !value);
        text.classList.toggle('text-ink', !!value);
      }
      this.el.toggleAttribute('aria-busy', this.pending.size > 0);
      if (this.pending.size) this.el.setAttribute('aria-busy', 'true');
      this.el.querySelector('[data-role="saving"]')?.classList.toggle('hidden', !this.pending.size);
    },
    start(value, atEnd = false, autofocus = true) {
      if (this.el.dataset.editable !== 'true' || this.active) return;
      this.active = true;
      // An unsuccessful draft keeps its original version until explicit Escape.
      const draft = this.draft || {value: this.el.dataset.value, version: this.el.dataset.version};
      this.original = this.el.dataset.value;
      this.version = draft.version;
      const input = this.control();
      if (!input) { this.active = false; return; }
      input.value = value === undefined ? draft.value : value;
      const combo = this.editor().querySelector('[data-hook="Combobox"]');
      combo?.dispatchEvent(new CustomEvent('bilibi:choice-value', {detail: {value: input.value}}));
      this.updated();
      if (!autofocus) return;
      const focus = this.editor().querySelector('[role="combobox"]') || input;
      focus.focus();
      if (focus.select) {
        if (value !== undefined || atEnd) focus.setSelectionRange(focus.value.length, focus.value.length);
        else focus.select();
      }
    },
    cancel(returnFocus) {
      this.active = false;
      this.draft = null;
      this.message('');
      this.updated();
      if (returnFocus) this.trigger()?.focus();
    },
    commit(returnFocus) {
      if (!this.active) return;
      const input = this.control();
      const value = this.long || this.choice ? input.value : input.value.trim();
      const unchanged = value === this.original;
      if (unchanged || (!value.trim() && !this.el.hasAttribute('data-allow-empty'))) {
        this.cancel(returnFocus);
        return;
      }
      const draft = {value, version: this.version, sequence: ++this.sequence};
      this.draft = draft;
      this.active = false;
      this.pending.add(draft);
      this.message('');
      this.updated();
      if (returnFocus) this.trigger()?.focus();
      // onReply is local metadata, never serialized onto the wire. It runs
      // after the matching server reply has patched the committed value/status.
      this.emit(this.el.dataset.saveEvent, {
        id: this.el.dataset.id, field: this.el.dataset.field,
        value, version: draft.version,
        onReply: (reply) => {
          this.pending.delete(draft);
          if (!this.el.isConnected) return;
          if (reply.type === 'error') this.message(reply.message || 'The change was not confirmed. Open the field to retry, or press Escape to discard the draft.');
          if (this.draft === draft && reply.type !== 'error' && this.el.dataset.status === 'saved') this.draft = null;
          this.updated();
        },
      });
    },
    message(value) {
      const message = this.el.querySelector('[data-role="connection"]');
      if (!message) return;
      message.textContent = value;
      message.classList.toggle('hidden', !value);
    },
  };
  for (const name of ['InlineEdit', 'InlineLongText', 'InlineChoice']) window.BilibiUI.registerHook(name, hook);
})();
