// The browser half of the flexible table (templates/flex_table.html,
// data-hook="FlexTable").
// The server renders the whole table; this hook owns only what a browser can
// do, and reports every intent as the table's one event with an `op` through
// `this.emit` (the transport's queued `bilibi:event`):
//
//   * Dragging a chip or a heading onto another reorders the columns (`move`).
//   * The zoom controls carry data-zoom-op and no data-event, so each press is
//     raised here as its own intent and the transport queues it: three presses
//     on "Taller rows" are three steps, and Normal straight after Compact lands.
//     Nothing is dropped while an earlier reply is in flight.
//   * The bar widths: the host CSP refuses a style attribute, and the patcher
//     removes the style it did not render, so data-bar is written through the
//     CSSOM after every patch, and only where it differs.
//   * The add-a-column box: the arrow keys walk its suggestions, Escape clears
//     what was typed, and Enter adds the best match (`add_typed`) instead of
//     asking for suggestions again.
//   * Choosing a lens closes its menu at once.
//
// The notch's aria-expanded is the one record of open (DisclosureDismiss owns
// it; the notch is data-live-ignore), so nothing here tracks open state.
(() => {
  const FlexTable = {
    mounted() {
      this.drag = null

      this.onDragStart = (event) => this.dragStart(event)
      this.onDragOver = (event) => this.dragOver(event)
      this.onDragLeave = (event) => this.dragLeave(event)
      this.onDrop = (event) => this.drop(event)
      this.onDragEnd = () => this.dragEnd()
      this.onKeyDown = (event) => this.addBarKeys(event)
      this.onClick = (event) => this.press(event)

      this.el.addEventListener('dragstart', this.onDragStart)
      this.el.addEventListener('dragover', this.onDragOver)
      this.el.addEventListener('dragleave', this.onDragLeave)
      this.el.addEventListener('drop', this.onDrop)
      this.el.addEventListener('dragend', this.onDragEnd)
      this.el.addEventListener('keydown', this.onKeyDown)
      this.el.addEventListener('click', this.onClick)

      this.applyBars()
    },

    updated() {
      this.applyBars()
    },

    destroyed() {
      clearTimeout(this.opReset)
      this.el.removeEventListener('dragstart', this.onDragStart)
      this.el.removeEventListener('dragover', this.onDragOver)
      this.el.removeEventListener('dragleave', this.onDragLeave)
      this.el.removeEventListener('drop', this.onDrop)
      this.el.removeEventListener('dragend', this.onDragEnd)
      this.el.removeEventListener('keydown', this.onKeyDown)
      this.el.removeEventListener('click', this.onClick)
    },

    push(payload) {
      this.emit(this.el.dataset.event, payload)
    },

    // --- zoom and lens ---------------------------------------------------

    // A step carries its direction and a preset its name, as data attributes
    // the server rendered. A disabled step is the end of the range.
    press(event) {
      const control = event.target.closest('[data-zoom-op]')
      if (control && !control.disabled) {
        const {zoomOp: op, dir, preset} = control.dataset
        this.push({op, ...(dir && {dir}), ...(preset && {preset})})
        return
      }
      // The transport sends the lens itself (data-event); its menu closes now.
      const lens = event.target.closest('[data-op="lens"]')
      const trigger = lens && lens.closest('[data-hook="DisclosureDismiss"]')?.querySelector('[aria-expanded]')
      if (trigger && trigger.getAttribute('aria-expanded') !== 'false') trigger.setAttribute('aria-expanded', 'false')
    },

    // --- bars (CSSOM, because the CSP refuses inline style) ---------------

    applyBars() {
      for (const bar of this.el.querySelectorAll('[data-bar]')) {
        const percent = parseFloat(bar.dataset.bar)
        const width = Number.isFinite(percent) ? `${percent}%` : '0%'
        if (bar.style.width !== width) bar.style.width = width
      }
    },

    // --- drag to reorder ---------------------------------------------------

    dragStart(event) {
      const source = event.target.closest('[data-chip], [data-header]')
      if (!source) return
      this.drag = {spec: source.dataset.chip || source.dataset.header}
      source.dataset.dragging = 'true'
      event.dataTransfer.effectAllowed = 'move'
      event.dataTransfer.setData('text/plain', this.drag.spec)
    },

    dragOver(event) {
      if (!this.drag) return
      const over = event.target.closest('[data-chip], [data-header]')
      if (!over) return
      event.preventDefault()
      event.dataTransfer.dropEffect = 'move'
      this.clearDropMarks()
      const spec = over.dataset.chip || over.dataset.header
      if (spec !== this.drag.spec) over.dataset.drop = this.before(event, over) ? 'before' : 'after'
    },

    dragLeave(event) {
      const left = event.target.closest && event.target.closest('[data-chip], [data-header]')
      if (left) delete left.dataset.drop
    },

    drop(event) {
      if (!this.drag) return
      event.preventDefault()
      const over = event.target.closest('[data-chip], [data-header]')
      const spec = this.drag.spec
      if (over) {
        const target = over.dataset.chip || over.dataset.header
        if (target !== spec) {
          const before = this.before(event, over)
          this.push({op: 'move', spec, before: before ? target : this.after(target)})
        }
      }
      this.dragEnd()
    },

    // The spec following `target` in the current order, or null at the end.
    // The chips are the order: the server renders one per column.
    after(target) {
      const specs = [...this.el.querySelectorAll('[data-chip]')].map((chip) => chip.dataset.chip)
      const index = specs.indexOf(target)
      return index >= 0 && index + 1 < specs.length ? specs[index + 1] : null
    },

    before(event, element) {
      const box = element.getBoundingClientRect()
      return event.clientX < box.left + box.width / 2
    },

    dragEnd() {
      this.clearDropMarks()
      for (const el of this.el.querySelectorAll('[data-dragging]')) delete el.dataset.dragging
      this.drag = null
    },

    clearDropMarks() {
      for (const el of this.el.querySelectorAll('[data-drop]')) delete el.dataset.drop
    },

    // --- the add-a-column box --------------------------------------------

    // The arrow keys walk the suggestions and Enter on one picks it (a
    // button). Escape clears what was typed. Enter in the box submits the
    // form, which the transport sends with its pending debounce cancelled;
    // the form's op says `add_typed` for that one submission.
    addBarKeys(event) {
      const input = event.target.closest('input[name=add]')
      const option = event.target.closest('[role=listbox] button')
      if (!input && !option) return
      const options = [...this.el.querySelectorAll('[role=listbox] button')]
      const index = options.indexOf(document.activeElement)
      if (event.key === 'ArrowDown' && options.length) {
        event.preventDefault()
        options[Math.min(index + 1, options.length - 1)].focus()
      } else if (event.key === 'ArrowUp' && options.length) {
        event.preventDefault()
        if (index <= 0) this.el.querySelector('input[name=add]').focus()
        else options[index - 1].focus()
      } else if (event.key === 'Escape' && input) {
        event.preventDefault()
        input.value = ''
        input.dispatchEvent(new Event('input', {bubbles: true}))
      } else if (event.key === 'Enter' && input && input.form) {
        const op = input.form.querySelector('input[name=op]')
        if (!op) return
        op.value = 'add_typed'
        // The implicit submission runs in this task; the next one restores
        // the op the box's typing raises.
        clearTimeout(this.opReset)
        this.opReset = setTimeout(() => { if (op.isConnected) op.value = 'suggest' }, 0)
      }
    },
  }

  window.BilibiUI.registerHook('FlexTable', FlexTable)
})()
