// Browser behaviour for the shared form controls in templates/forms.html:
// CodeInput, ColourInput, Combobox, SecretReveal, SecretClear, SecretStored and
// DisclosureDismiss, MultiSelect (plus the dashboard's DashboardSort, the form Fold and
// the CountdownButton's Countdown), adapted from Bilimbi's Phoenix hooks (apps/web/assets/js
// at the pinned source) with the LiveView JS commands folded into the hooks.
// Each registers with the shared `data-hook` runtime in ui.js
// (BilibiUI.registerHook), which mounts a control when it enters the
// document, calls updated when its subtree is patched and destroyed when it
// leaves. The server owns every value and the option vocabulary; a hook owns
// only transient state (open, query, caret, masked-or-shown). A hook reports
// intent with `this.emit(event, values)`, the runtime's bubbling
// `bilibi:event` CustomEvent, and hears a server push as a `window` event
// `bilibi:<name>` with the payload in detail. ui.js must load first.
(() => {
  // The server owns opening/closing; this hook owns keyboard and focus only.
  window.BilibiUI.registerHook("InlineForm", {
    mounted() {
      this.keydown = event => {
        if (this.el.dataset.editing !== "true") return
        if (event.key === "Escape") {
          event.preventDefault()
          event.stopPropagation()
          this.emit(this.el.dataset.cancelEvent, {key: this.el.dataset.key})
        } else if (event.key === "Enter" && !event.isComposing &&
                   event.target.matches("input:not([type=button]):not([type=submit])")) {
          event.preventDefault()
          this.el.querySelector("form")?.requestSubmit()
        }
      }
      this.el.addEventListener("keydown", this.keydown)
      this.updated()
    },
    updated() {
      const editing = this.el.dataset.editing === "true"
      if (editing !== this.editing) {
        this.editing = editing
        const target = editing ? this.el.querySelector("input:not([type=hidden]):not([disabled]), select, textarea") : this.el.querySelector("button")
        if (editing || this.wasEditing) target?.focus()
        this.wasEditing = editing
      }
    },
    destroyed() { this.el.removeEventListener("keydown", this.keydown) },
  })

  // One native input is the keyboard and accessibility contract. Decorative
  // boxes follow its caret, so editing never adds eight tab stops or labels.
  const CodeInput = {
    mounted() {
      this.input = this.el.querySelector("input")
      this.boxes = [...this.el.querySelectorAll("[data-code-index]")]
      this.length = Number(this.el.dataset.codeLength)
      this.normalize = value => (this.el.hasAttribute("data-code-numeric") ?
        value.replace(/[^0-9]/g, "") : value.toUpperCase().replace(/[\s-]/g, "")).slice(0, this.length)
      // Writes only what changed: the runtime reports a child mutation inside
      // the control as an update, and rewriting an unchanged text node would
      // report another.
      this.paint = () => {
        const active = Math.min(this.input.selectionStart ?? 0, this.length - 1)
        this.boxes.forEach((box, index) => {
          const text = this.input.value[index] || ""
          if (box.textContent !== text) box.textContent = text
          box.toggleAttribute("data-active", document.activeElement === this.input &&
            (index === active || (index >= this.input.selectionStart && index < this.input.selectionEnd)))
          box.toggleAttribute("data-invalid", this.input.getAttribute("aria-invalid") === "true")
        })
      }
      this.onInput = () => {
        const caret = this.normalize(this.input.value.slice(0, this.input.selectionStart)).length
        this.input.value = this.normalize(this.input.value)
        this.input.setSelectionRange(caret, caret)
        this.paint()
      }
      this.onPaste = event => {
        const text = event.clipboardData?.getData("text")
        if (text == null) return
        event.preventDefault()
        const start = this.input.selectionStart
        const end = this.input.selectionEnd
        const code = this.normalize(text)
        // A whole code replaces the field even when pasted midway through it.
        this.input.value = code.length === this.length ? code :
          this.normalize(this.input.value.slice(0, start) + code + this.input.value.slice(end))
        const caret = code.length === this.length ? this.length : Math.min(start + code.length, this.length)
        this.input.setSelectionRange(caret, caret)
        this.input.dispatchEvent(new Event("input", {bubbles: true}))
      }
      this.onClick = event => {
        // The transparent native input remains hit-testable for touch paste.
        // Preserve ranges created by dragging, Shift-click or multi-click.
        if (event.target === this.input && (this.input.selectionStart !== this.input.selectionEnd ||
          event.shiftKey || event.detail > 1)) { this.paint(); return }
        const box = event.target === this.input ? this.boxes.find(box => {
          const rect = box.getBoundingClientRect()
          return event.clientX >= rect.left && event.clientX <= rect.right
        }) : event.target.closest("[data-code-index]")
        if (!box) return
        this.input.focus()
        const index = Math.min(Number(box.dataset.codeIndex), this.input.value.length)
        this.input.setSelectionRange(index, index)
        this.paint()
      }
      this.input.value = this.normalize(this.input.value)
      this.input.classList.add("absolute", "inset-0", "w-full", "opacity-0")
      this.el.querySelector("[data-code-boxes]").hidden = false
      this.input.addEventListener("input", this.onInput)
      this.input.addEventListener("paste", this.onPaste)
      for (const event of ["focus", "blur", "keyup", "select"]) this.input.addEventListener(event, this.paint)
      this.el.addEventListener("click", this.onClick)
      this.paint()
    },
    updated() {
      this.input.classList.add("absolute", "inset-0", "w-full", "opacity-0")
      this.el.querySelector("[data-code-boxes]").hidden = false
      this.input.value = this.normalize(this.input.value)
      this.paint()
    },
    destroyed() {
      this.input.removeEventListener("input", this.onInput)
      this.input.removeEventListener("paste", this.onPaste)
      for (const event of ["focus", "blur", "keyup", "select"]) this.input.removeEventListener(event, this.paint)
      this.el.removeEventListener("click", this.onClick)
    },
  }

  // Select-only colour combobox. The native select is the only submitted
  // control, including when JavaScript is unavailable. The closed field and
  // each option show the name inside that colour's banner chip. Unlike a
  // searchable combobox, typing moves focus among named colours without
  // editing the value.
  const ColourInput = {
    mounted() {
      this.select = this.el.querySelector('[data-colour-select]')
      this.trigger = this.el.querySelector('[data-colour-trigger]')
      this.list = this.el.querySelector('[role="listbox"]')
      this.label = this.el.querySelector('label')
      this.open = false
      this.active = 0
      this.query = ''
      this.options = () => Array.from(this.list.querySelectorAll('[role="option"]'))
      this.sync = () => {
        this.select.hidden = true
        this.trigger.hidden = false
        this.trigger.disabled = this.select.matches(':disabled') || this.select.hasAttribute('readonly')
        if (this.trigger.disabled) this.open = false
        this.label.htmlFor = this.trigger.id
        const options = this.options()
        const selected = options.find(option => option.dataset.value === this.select.value) || options[0]
        // The shared observer calls updated after child changes. Keep this
        // repaint idempotent or an unchanged preview would update forever.
        const preview = this.el.querySelector('[data-colour-preview]')
        const chip = selected.querySelector('[data-option-label]')
        const text = chip.textContent
        if (preview.textContent !== text) preview.textContent = text
        if (preview.className !== chip.className) preview.className = chip.className
        options.forEach(option => option.setAttribute('aria-selected', String(option === selected)))
        if (!this.open) this.active = options.indexOf(selected)
        this.active = Math.min(this.active, options.length - 1)
        this.paint()
      }
      this.paint = () => {
        this.trigger.setAttribute('aria-expanded', String(this.open))
        this.list.hidden = !this.open
        this.options().forEach((option, index) => option.toggleAttribute('data-active', this.open && index === this.active))
        if (this.open) {
          const option = this.options()[this.active]
          this.trigger.setAttribute('aria-activedescendant', option.id)
          option.scrollIntoView({block: 'nearest'})
        } else this.trigger.removeAttribute('aria-activedescendant')
      }
      this.setOpen = open => {
        this.open = open && !this.trigger.disabled
        this.query = ''
        this.active = Math.max(0, this.options().findIndex(option => option.dataset.value === this.select.value))
        this.paint()
      }
      this.commit = option => {
        if (this.trigger.disabled) return
        this.select.value = option.dataset.value
        this.setOpen(false)
        this.sync()
        this.select.dispatchEvent(new Event('input', {bubbles: true}))
        this.select.dispatchEvent(new Event('change', {bubbles: true}))
      }
      this.onClick = () => this.setOpen(!this.open)
      this.onKey = event => {
        const count = this.options().length
        if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
          event.preventDefault()
          const wasOpen = this.open
          if (!wasOpen) this.setOpen(true)
          if (event.key === 'Home') this.active = 0
          else if (event.key === 'End') this.active = count - 1
          else if (wasOpen) this.active = (this.active + (event.key === 'ArrowDown' ? 1 : -1) + count) % count
          this.paint()
        } else if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault()
          if (this.open) this.commit(this.options()[this.active])
          else this.setOpen(true)
        } else if (event.key === 'Escape' && this.open) {
          event.preventDefault()
          event.stopPropagation()
          this.setOpen(false)
        } else if (event.key === 'Tab') this.setOpen(false)
        else if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
          event.preventDefault()
          if (!this.open) this.setOpen(true)
          clearTimeout(this.queryTimer)
          this.query += event.key.toLocaleLowerCase()
          const match = this.options().findIndex(option => option.textContent.trim().toLocaleLowerCase().startsWith(this.query))
          if (match >= 0) this.active = match
          this.paint()
          this.queryTimer = setTimeout(() => { this.query = '' }, 700)
        }
      }
      this.onOption = event => {
        const option = event.target.closest('[role="option"]')
        if (option && this.list.contains(option)) this.commit(option)
      }
      this.onPointer = event => {
        if (event.target.closest('[role="option"]')) event.preventDefault()
      }
      this.onOutside = event => { if (!this.el.contains(event.target)) this.setOpen(false) }
      this.onBlur = event => { if (!this.el.contains(event.relatedTarget)) this.setOpen(false) }
      this.onReset = () => { this.resetTimer = setTimeout(() => { this.setOpen(false); this.sync() }, 0) }
      this.onChange = () => this.sync()
      this.form = this.select.form
      this.trigger.addEventListener('click', this.onClick)
      this.trigger.addEventListener('keydown', this.onKey)
      this.list.addEventListener('pointerdown', this.onPointer)
      this.list.addEventListener('click', this.onOption)
      this.el.addEventListener('focusout', this.onBlur)
      this.select.addEventListener('change', this.onChange)
      this.form?.addEventListener('reset', this.onReset)
      document.addEventListener('pointerdown', this.onOutside)
      this.sync()
    },
    updated() { this.sync() },
    destroyed() {
      clearTimeout(this.queryTimer)
      clearTimeout(this.resetTimer)
      this.trigger.removeEventListener('click', this.onClick)
      this.trigger.removeEventListener('keydown', this.onKey)
      this.list.removeEventListener('pointerdown', this.onPointer)
      this.list.removeEventListener('click', this.onOption)
      this.el.removeEventListener('focusout', this.onBlur)
      this.select.removeEventListener('change', this.onChange)
      this.form?.removeEventListener('reset', this.onReset)
      document.removeEventListener('pointerdown', this.onOutside)
    }
  }

  // Client-side filtering and keyboard interaction for the combobox.
  //
  // The server owns the hidden form value and the option vocabulary. This hook
  // owns only the transient query, open state and active option, so typing
  // does not cause a round trip or lose the caret: the filter input's own
  // input and change events stop here, before an enclosing form hears them. A
  // choice is committed by copying its value to the hidden input and
  // dispatching a normal bubbling change event; forms therefore keep their
  // existing change and submit contracts. Typing highlights the first match,
  // and Enter while the list is open commits the highlighted option or, with
  // none, does nothing, so it never submits the form.
  const Combobox = {
    mounted() {
      this.input = this.el.querySelector('[role="combobox"]')
      this.valueInput = document.getElementById(this.el.dataset.valueId)
      this.listbox = this.el.querySelector('[role="listbox"]')
      this.noMatches = this.el.querySelector('[id$="-no-matches"]')
      this.clearButton = this.el.querySelector('[id$="-clear"]')
      this.open = false
      this.activeIndex = -1
      this.selectedValue = this.valueInput?.value || ""
      this.selectedLabel = this.input?.value || ""

      this.options = () => Array.from(this.listbox?.querySelectorAll('[role="option"]') || [])

      this.filteredOptions = () => this.options().filter(option => !option.hidden)

      this.setOpen = open => {
        this.open = open
        this.input.setAttribute("aria-expanded", String(open))
        this.listbox.hidden = !open

        if (!open) {
          this.input.removeAttribute("aria-activedescendant")
          this.activeIndex = -1
        }
      }

      this.applyFilter = () => {
        const selectionIsActive = this.input.selectionStart === 0 &&
          this.input.selectionEnd === this.input.value.length
        const query = selectionIsActive ? "" : this.input.value.trim().toLocaleLowerCase()
        const options = this.options()

        options.forEach(option => {
          const label = option.dataset.label.toLocaleLowerCase()
          const value = option.dataset.value.toLocaleLowerCase()
          option.hidden = query !== "" && !label.includes(query) && !value.includes(query)
        })

        const filtered = this.filteredOptions()
        this.noMatches.hidden = options.length === 0 || filtered.length !== 0

        if (this.activeIndex >= filtered.length) this.activeIndex = filtered.length - 1
        this.renderActive()
      }

      this.renderActive = () => {
        const filtered = this.filteredOptions()

        this.options().forEach(option => {
          const active = filtered[this.activeIndex] === option
          option.classList.toggle("bg-brand-surface", active)
          option.setAttribute("aria-selected", String(option.dataset.value === this.selectedValue))
        })

        const active = filtered[this.activeIndex]
        if (active) {
          this.input.setAttribute("aria-activedescendant", active.id)
          active.scrollIntoView({block: "nearest"})
        } else {
          this.input.removeAttribute("aria-activedescendant")
        }
      }

      this.openList = selectQuery => {
        if (!this.open) {
          this.setOpen(true)
          this.activeIndex = -1
          if (selectQuery) this.input.select()
        }

        this.applyFilter()
      }

      this.restore = () => {
        this.selectedValue = this.valueInput?.value || ""
        const selected = this.options().find(option => option.dataset.value === this.selectedValue)
        this.selectedLabel = selected?.dataset.label || ""
        this.input.value = this.selectedLabel
      }

      this.close = () => {
        this.restore()
        this.setOpen(false)
      }

      this.commit = option => {
        if (!option) return

        this.selectedValue = option.dataset.value
        this.selectedLabel = option.dataset.label
        this.input.value = this.selectedLabel
        this.valueInput.value = this.selectedValue
        this.setOpen(false)
        this.valueInput.dispatchEvent(new Event("input", {bubbles: true}))
        this.valueInput.dispatchEvent(new Event("change", {bubbles: true}))
      }

      this.cancel = () => {
        const wasOpen = this.open
        this.close()

        if (!wasOpen || !this.el.dataset.cancelEvent) return

        this.emit(this.el.dataset.cancelEvent)
      }

      this.onInput = event => {
        event.stopPropagation()
        if (!this.open) this.setOpen(true)
        this.activeIndex = 0
        this.applyFilter()
      }
      this.onChange = event => event.stopPropagation()
      this.onFocus = () => this.openList(true)
      this.onKeyDown = event => {
        const filtered = this.filteredOptions()

        if (event.key === "ArrowDown" || event.key === "ArrowUp") {
          event.preventDefault()
          this.openList(false)
          const delta = event.key === "ArrowDown" ? 1 : -1
          this.activeIndex = filtered.length === 0
            ? -1
            : this.activeIndex < 0
              ? (delta === 1 ? 0 : filtered.length - 1)
              : (this.activeIndex + delta + filtered.length) % filtered.length
          this.renderActive()
        } else if (event.key === "Enter") {
          if (this.open) {
            event.preventDefault()
            this.commit(filtered[this.activeIndex])
          }
        } else if (event.key === "Escape") {
          if (this.open) {
            event.preventDefault()
            this.cancel()
          }
        }
      }

      this.onOptionMouseDown = event => {
        event.preventDefault()
        this.commit(event.currentTarget)
      }

      this.bindOptions = () => {
        this.options().forEach(option => {
          option.removeEventListener("mousedown", this.onOptionMouseDown)
          option.addEventListener("mousedown", this.onOptionMouseDown)
        })
      }

      this.bindClear = () => {
        this.clearButton?.removeEventListener("mousedown", this.onClearMouseDown)
        this.clearButton?.removeEventListener("click", this.onClear)
        this.clearButton = this.el.querySelector('[id$="-clear"]')
        this.clearButton?.addEventListener("mousedown", this.onClearMouseDown)
        this.clearButton?.addEventListener("click", this.onClear)
      }

      this.onDocumentPointerDown = event => {
        if (!this.el.contains(event.target) && this.open) this.cancel()
      }

      this.onFocusOut = event => {
        if (event.relatedTarget && this.el.contains(event.relatedTarget)) return
        requestAnimationFrame(() => {
          if (this.open && !this.el.contains(document.activeElement)) this.cancel()
        })
      }

      this.onClear = event => {
        event.preventDefault()
        this.selectedValue = ""
        this.selectedLabel = ""
        this.input.value = ""
        this.valueInput.value = ""
        this.valueInput.dispatchEvent(new Event("input", {bubbles: true}))
        this.valueInput.dispatchEvent(new Event("change", {bubbles: true}))
        this.input.focus()
        this.openList(false)
      }

      this.onClearMouseDown = event => event.preventDefault()

      this.input.addEventListener("input", this.onInput)
      this.input.addEventListener("change", this.onChange)
      this.input.addEventListener("focus", this.onFocus)
      this.input.addEventListener("keydown", this.onKeyDown)
      this.el.addEventListener("focusout", this.onFocusOut)
      this.bindOptions()
      this.bindClear()
      document.addEventListener("pointerdown", this.onDocumentPointerDown)

      this.applyFilter()
    },

    updated() {
      this.bindOptions()
      this.bindClear()
      if (!this.open) this.restore()
      this.setOpen(this.open)
      this.applyFilter()
    },

    destroyed() {
      this.input.removeEventListener("input", this.onInput)
      this.input.removeEventListener("change", this.onChange)
      this.input.removeEventListener("focus", this.onFocus)
      this.input.removeEventListener("keydown", this.onKeyDown)
      this.el.removeEventListener("focusout", this.onFocusOut)
      this.options().forEach(option => option.removeEventListener("mousedown", this.onOptionMouseDown))
      this.clearButton?.removeEventListener("click", this.onClear)
      this.clearButton?.removeEventListener("mousedown", this.onClearMouseDown)
      document.removeEventListener("pointerdown", this.onDocumentPointerDown)
    },
  }

  // Companion to the reveal option of a password input.
  //
  // The input's `type` is the only record of masked-or-shown: the click flips
  // it, and everything else is derived from it -- the button's accessible
  // name, its title and which glyph shows -- by one read of the attribute, so
  // however many clicks land the state cannot drift, and a stored secret
  // that sets `type` itself (SecretStored) is reflected the same way.
  //
  // A pointer press on the toggle must not pull focus out of the input, and
  // the caret must survive the type swap (browsers reset the selection when
  // `type` changes), so the user can look, then keep typing where they were.
  // Keyboard activation still focuses the button as usual.
  const SecretReveal = {
    mounted() {
      this.input = document.getElementById(this.el.getAttribute("aria-controls"))
      this.showGlyph = document.getElementById(`${this.el.id}-show`)
      this.hideGlyph = document.getElementById(`${this.el.id}-hide`)

      this.sync = () => {
        const shown = this.input.getAttribute("type") === "text"
        const labels = this.el.dataset

        this.el.setAttribute("aria-label", shown ? labels.hideLabel : labels.showLabel)
        this.el.setAttribute("title", shown ? labels.hideTitle : labels.showTitle)
        this.showGlyph.classList.toggle("hidden", shown)
        this.hideGlyph.classList.toggle("hidden", !shown)
      }

      this.onMouseDown = event => event.preventDefault()

      this.onClick = () => {
        const focused = document.activeElement === this.input
        const {selectionStart, selectionEnd} = this.input
        this.input.setAttribute("type", this.input.getAttribute("type") === "text" ? "password" : "text")
        if (focused) this.input.setSelectionRange(selectionStart, selectionEnd)
      }

      this.el.addEventListener("mousedown", this.onMouseDown)
      this.el.addEventListener("click", this.onClick)

      this.masking = new MutationObserver(this.sync)
      this.masking.observe(this.input, {attributeFilter: ["type"]})

      this.sync()
    },

    updated() {
      this.sync()
    },

    destroyed() {
      this.masking.disconnect()
      this.el.removeEventListener("mousedown", this.onMouseDown)
      this.el.removeEventListener("click", this.onClick)
    },
  }

  // Clear a stored secret's keep-current mask in the input property that
  // forms submit. Changing the value attribute would leave the submitted
  // value intact.
  const SecretClear = {
    mounted() {
      this.input = document.getElementById(this.el.dataset.inputId)
      this.clear = () => {
        this.input.value = ""
        this.input.dispatchEvent(new Event("secret:cleared"))
        this.input.focus()
      }
      this.el.addEventListener("click", this.clear)
    },

    destroyed() {
      this.el.removeEventListener("click", this.clear)
    },
  }

  // Only a successful server-side reauthentication sends plaintext to this
  // hook, as a `bilibi:secret:reveal` window event ({id, value, duration_ms})
  // dispatched by the transport. No plaintext
  // is rendered in initial HTML or kept in a hook field. The input property
  // is reset locally even if the connection drops before the timeout. A value
  // the user types or clears during the window is kept, never re-masked.
  const SecretStored = {
    mounted() {
      this.mask = this.el.value
      this.stop = () => {
        clearTimeout(this.timer)
        this.revealing = false
        this.el.type = "password"
      }
      this.onReveal = ({detail}) => {
        const {id, value, duration_ms} = detail || {}
        if (id !== this.el.id) return
        clearTimeout(this.timer)
        this.el.value = value
        this.el.type = "text"
        this.revealing = true
        this.timer = setTimeout(() => this.hide(), duration_ms)
      }
      this.el.addEventListener("secret:cleared", this.stop)
      this.el.addEventListener("input", this.stop)
      window.addEventListener("bilibi:secret:reveal", this.onReveal)
    },

    hide() {
      if (this.revealing) this.el.value = this.mask
      this.stop()
    },

    destroyed() {
      this.hide()
      this.el.removeEventListener("secret:cleared", this.stop)
      this.el.removeEventListener("input", this.stop)
      window.removeEventListener("bilibi:secret:reveal", this.onReveal)
    },
  }

  // Companion to the multi-select and the record history disclosure. The
  // first `[aria-expanded]` inside the wrapper is the trigger whose state is
  // the single record of open; the list and the chevron derive from it in
  // CSS, so they cannot disagree with it. This hook writes that one attribute
  // on every path: the trigger's click toggles it, and a click outside the
  // field, focus leaving the field, and Escape close it.
  //
  // Escape listens on `window`, not the field: Safari and macOS Firefox blur
  // without focusing the button being pressed, so a list opened by mouse
  // there is open while focus sits on `body`, and a listener on the field
  // would never see the key. It is attached only while the list is open, so
  // no closed field is watching keys and no other Escape handler on the page
  // loses one. Reaching that far means the key also arrives from a search
  // box the user is typing in, so only a press with focus already inside the
  // field returns focus to the trigger; everywhere else the list closes and
  // the caret stays where the user put it.
  //
  // A pointer press starting inside the field is not focus leaving it, even
  // when the browser reports no new target: the click that press belongs to
  // already owns the toggle, so dismissing on the blur would close the list
  // and let the same click reopen it. The press ends on the release, which
  // arrives even when the click never does.
  //
  // A wrapper marked `data-keep-on-blur` is a bar a person works in rather
  // than a list they pick from: it closes on its own control and on Escape,
  // and on a click outside it only if it also carries `data-click-away`.
  const DisclosureDismiss = {
    mounted() {
      this.pressing = false
      this.listening = false
      this.trigger = this.el.querySelector("[aria-expanded]")
      this.keepOnBlur = this.el.hasAttribute("data-keep-on-blur")
      this.clickAway = !this.keepOnBlur || this.el.hasAttribute("data-click-away")

      this.isOpen = () => this.trigger.getAttribute("aria-expanded") === "true"
      this.dismiss = () => this.trigger.setAttribute("aria-expanded", "false")
      this.escape = () => {
        this.dismiss()
        this.trigger.focus()
      }
      this.toggle = () => this.trigger.setAttribute("aria-expanded", this.isOpen() ? "false" : "true")

      this.onPointerDown = () => (this.pressing = true)
      this.onPressEnd = () => (this.pressing = false)

      this.onKeyDown = e => {
        if (e.key !== "Escape") return

        if (this.el.contains(document.activeElement)) this.escape()
        else this.dismiss()
      }

      this.onClickAway = e => {
        if (!this.el.contains(e.target)) this.dismiss()
      }

      this.onFocusOut = e => {
        if (this.keepOnBlur) return

        if (this.pressing) {
          this.pressing = false
          return
        }

        if (e.relatedTarget && this.el.contains(e.relatedTarget)) return

        this.dismiss()
      }

      this.syncListeners = () => {
        const open = this.isOpen()
        if (open === this.listening) return

        this.listening = open

        if (open) {
          window.addEventListener("keydown", this.onKeyDown)
          if (this.clickAway) document.addEventListener("click", this.onClickAway)
        } else {
          window.removeEventListener("keydown", this.onKeyDown)
          document.removeEventListener("click", this.onClickAway)
        }
      }

      this.expansion = new MutationObserver(this.syncListeners)
      this.expansion.observe(this.trigger, {attributeFilter: ["aria-expanded"]})

      this.trigger.addEventListener("click", this.toggle)
      this.el.addEventListener("pointerdown", this.onPointerDown)
      this.el.addEventListener("focusout", this.onFocusOut)
      window.addEventListener("pointerup", this.onPressEnd)
      window.addEventListener("pointercancel", this.onPressEnd)

      this.syncListeners()
    },

    destroyed() {
      this.expansion.disconnect()
      this.trigger.removeEventListener("click", this.toggle)
      this.el.removeEventListener("pointerdown", this.onPointerDown)
      this.el.removeEventListener("focusout", this.onFocusOut)
      window.removeEventListener("keydown", this.onKeyDown)
      document.removeEventListener("click", this.onClickAway)
      window.removeEventListener("pointerup", this.onPressEnd)
      window.removeEventListener("pointercancel", this.onPressEnd)
    },
  }

  // Bilimbi multi_select disclosure with Bilibi's inset chip presentation.
  // Values remain the owner's; overflow is measured from the actual field width.
  const MultiSelect = {
    mounted() {
      DisclosureDismiss.mounted.call(this)
      this.paintChips = () => {
        const row = this.el.querySelector("[data-chip-row]")
        if (!row || !row.clientWidth) return
        const chips = [...row.querySelectorAll("[data-selected-chip]")]
        const marker = row.querySelector("[data-chip-overflow]")
        const gap = parseFloat(getComputedStyle(row).columnGap) || 0
        const widths = chips.map(chip => chip.getBoundingClientRect().width)
        const total = widths.reduce((sum, width) => sum + width, 0) + Math.max(0, chips.length - 1) * gap
        let visible = chips.length
        if (!this.el.hasAttribute("data-wrap-chips") && total > row.clientWidth) {
          const budget = row.clientWidth - parseFloat(getComputedStyle(marker).width) - gap
          let used = 0
          visible = 0
          for (const width of widths) {
            const next = used + (visible ? gap : 0) + width
            if (next > budget) break
            used = next
            visible++
          }
        }
        chips.forEach((chip, index) => {
          const hidden = index >= visible
          if (chip.hasAttribute("data-overflow") !== hidden) chip.toggleAttribute("data-overflow", hidden)
          if (chip.getAttribute("aria-hidden") !== String(hidden)) chip.setAttribute("aria-hidden", String(hidden))
        })
        const hidden = visible === chips.length
        if (marker.hidden !== hidden) marker.hidden = hidden
        const title = hidden ? "" : `${chips.length - visible} more selected`
        if (marker.title !== title) marker.title = title
      }
      this.scheduleChips = () => {
        if (this.chipFrame) return
        this.chipFrame = requestAnimationFrame(() => {
          this.chipFrame = null
          this.paintChips()
        })
      }
      this.removeChip = event => {
        const button = event.target.closest(".ui-multi-remove")
        if (!button || button.disabled) return
        if (!button.hasAttribute("data-remove-value")) {
          this.trigger.focus()
          return
        }
        const input = [...this.el.querySelectorAll('input[type="checkbox"]')]
          .find(option => option.value === button.dataset.removeValue)
        if (!input || input.disabled) return
        input.checked = false
        input.dispatchEvent(new Event("change", {bubbles: true}))
        this.trigger.focus()
      }
      this.el.addEventListener("click", this.removeChip)
      this.chipResize = new ResizeObserver(this.scheduleChips)
      this.chipResize.observe(this.el)
      this.scheduleChips()
    },
    updated() { this.scheduleChips?.() },
    destroyed() {
      DisclosureDismiss.destroyed.call(this)
      this.chipResize.disconnect()
      cancelAnimationFrame(this.chipFrame)
      this.el.removeEventListener("click", this.removeChip)
    },
  }

  // Drag-to-reorder for the dashboard widget grid (Bilimbi DashboardSort).
  // Server-driven: the hook only computes the DOM order after a drop and
  // reports it as `reorder_widgets`; the dashboard validates the order is a
  // permutation of its current widget ids before persisting anything, so a
  // stale or forged report changes nothing. Dragging starts only from a card's
  // drag handle (the one draggable control); HTML5 drag and drop has no touch
  // support, so the move buttons remain the accessible reorder path.
  const DashboardSort = {
    mounted() {
      this.dragged = null
      this.onDragStart = event => {
        const item = event.target.closest("[data-widget-id]")
        if (!item || !this.el.contains(item)) return
        this.dragged = item
        event.dataTransfer.effectAllowed = "move"
        event.dataTransfer.setData("text/plain", item.dataset.widgetId)
        item.classList.add("opacity-40")
      }
      this.onDragOver = event => {
        if (!this.dragged) return
        event.preventDefault()
        event.dataTransfer.dropEffect = "move"
        const target = event.target.closest("[data-widget-id]")
        if (!target || target === this.dragged || !this.el.contains(target)) return
        // Before or after the hovered card by the pointer's half of it, so the
        // card follows the cursor the way people expect.
        const rect = target.getBoundingClientRect()
        const before = event.clientY < rect.top + rect.height / 2
        target.parentNode.insertBefore(this.dragged, before ? target : target.nextSibling)
      }
      this.onDrop = event => {
        if (this.dragged) event.preventDefault()
      }
      this.onDragEnd = () => {
        if (!this.dragged) return
        this.dragged.classList.remove("opacity-40")
        this.dragged = null
        this.emit("reorder_widgets", {ids: [...this.el.querySelectorAll("[data-widget-id]")].map(item => item.dataset.widgetId)})
      }
      this.el.addEventListener("dragstart", this.onDragStart)
      this.el.addEventListener("dragover", this.onDragOver)
      this.el.addEventListener("drop", this.onDrop)
      this.el.addEventListener("dragend", this.onDragEnd)
    },

    destroyed() {
      this.el.removeEventListener("dragstart", this.onDragStart)
      this.el.removeEventListener("dragover", this.onDragOver)
      this.el.removeEventListener("drop", this.onDrop)
      this.el.removeEventListener("dragend", this.onDragEnd)
    },
  }

  // A Fold (components.html `fold`) is a native details whose open state the
  // person owns; the patcher leaves it alone. The server marks data-open when
  // the fold must show (an error inside it), and this opens it on that change
  // only, so closing it again by hand sticks until the next new reason.
  const Fold = {
    mounted() {
      this.forced = this.el.hasAttribute("data-open")
    },

    updated() {
      const forced = this.el.hasAttribute("data-open")
      if (forced && !this.forced && !this.el.open) this.el.open = true
      this.forced = forced
    },
  }

  // A button usable again after a wait (CountdownButton): disabled, with the
  // seconds left after its label, until the wait the server rendered in
  // data-seconds ends. A new value from the server restarts it. The server
  // refuses an early use, so the button works without this.
  const Countdown = {
    mounted() {
      this.start()
    },
    updated() {
      if (this.el.dataset.seconds !== this.seen) this.start()
    },
    destroyed() {
      clearInterval(this.timer)
    },
    start() {
      clearInterval(this.timer)
      this.seen = this.el.dataset.seconds
      const button = this.el.querySelector("button")
      const text = this.el.querySelector("[data-countdown-text]")
      if (!button || !text) return
      const until = Date.now() + Math.max(0, Number(this.seen) || 0) * 1000
      const tick = () => {
        const left = Math.ceil((until - Date.now()) / 1000)
        const label = left > 0 ? ` (${left} s)` : ""
        // Writes only what changed, as CodeInput does.
        if (text.textContent !== label) text.textContent = label
        if (button.disabled !== left > 0) button.disabled = left > 0
        if (left <= 0) clearInterval(this.timer)
      }
      tick()
      if (Date.now() < until) this.timer = setInterval(tick, 1000)
    },
  }

  const registry = window.BilibiUI
  if (!registry) throw new Error("hooks.js needs ui.js loaded first")
  for (const [name, hook] of Object.entries({CodeInput, ColourInput, Combobox, SecretReveal, SecretClear, SecretStored, DisclosureDismiss, MultiSelect, DashboardSort, Fold, Countdown})) {
    registry.registerHook(name, hook)
  }
})()

;(() => {
  // Graphic's catalogue reports the actual clipboard result. Never claim a
  // successful copy merely because the tile was clicked.
  class ClipboardCopy {
    mounted() {
      this.onClick = async (event) => {
        const tile = event.target.closest('[data-copy-text]');
        if (!tile || !this.el.contains(tile)) return;
        let copied = false;
        try { await navigator.clipboard.writeText(tile.dataset.copyText); copied = true; } catch { /* permission refusal is a visible server state */ }
        this.emit(this.el.dataset.copyEvent || 'icon-copied', {text: tile.dataset.copyText, copied});
      };
      this.el.addEventListener('click', this.onClick);
    }
    destroyed() { this.el.removeEventListener('click', this.onClick); }
  }
  window.BilibiUI.registerHook('ClipboardCopy', ClipboardCopy.prototype);
})();

;(() => {
// Keeps every rendered instant on the saved display mode.
//
// A mode change has to reach timestamps that are already on screen, including
// rows a LiveView stream handed to the DOM and never re-renders. So the shell
// publishes the live mode on #app-shell and this hook re-applies it in place.
//
// The server decides `company` and `utc` and writes both strings onto the
// element, so those two modes are a copy, never a re-format: the browser
// cannot disagree with the server about a string it did not produce. `local`
// is the one mode the server cannot decide — it does not know the browser's
// zone — so it is formatted here, in the reader's own locale and hour cycle.
const SHELL = "#app-shell"

// Every mounted instant, so one shell change repaints all of them without
// each element observing the shell separately.
const instances = new Set()

// One observer for the whole page, armed on the first mount. It re-arms if the
// shell element is ever replaced rather than patched, so a stale observer
// cannot leave every timestamp silently frozen on the previous mode.
let observer = null
let observed = null

const shell = () => document.querySelector(SHELL)

const liveMode = () => shell()?.dataset.displayMode || null

const observeShell = () => {
  const el = shell()
  if (!el || observed === el) return
  observer?.disconnect()
  observer = new MutationObserver(() => {
    for (const hook of instances) hook.renderDateTime()
  })
  observer.observe(el, {attributes: true, attributeFilter: ["data-display-mode"]})
  observed = el
}

// `local` renders the instant the way the reader's own device would: their
// locale orders the fields and chooses the hour cycle. The zone is named on
// `datetime` only, where the label qualifies a full date and time.
const formatLocal = (value, timeZone, format, precision) => {
  const options = {timeZone}

  if (format !== "time") {
    Object.assign(options, {day: "2-digit", month: "2-digit", year: "numeric"})
  }

  if (format !== "date") {
    Object.assign(options, {hour: "2-digit", minute: "2-digit"})
    if (precision === "second") options.second = "2-digit"
  }

  if (format === "datetime") options.timeZoneName = "short"

  return new Intl.DateTimeFormat(undefined, options).format(value)
}

const DateTime = {
  mounted() {
    instances.add(this)
    observeShell()
    this.renderDateTime()
  },

  updated() {
    this.renderDateTime()
  },

  destroyed() {
    instances.delete(this)

    // With nothing left to repaint, stop observing. Otherwise the observer
    // keeps a strong reference to a detached #app-shell — the root of the
    // whole previous page — after navigating to a screen that carries no
    // instants and so mounts no hook to re-arm it.
    //
    // Conditioned on the set being empty rather than on destroyed() alone:
    // during LiveView navigation the incoming view's hooks mount BEFORE the
    // outgoing view's are destroyed, so live instances are still registered
    // here and an observer they depend on is never torn down underneath them.
    if (instances.size === 0) {
      observer?.disconnect()
      observer = null
      observed = null
    }
  },

  renderDateTime() {
    const value = new Date(this.el.dateTime)

    if (Number.isNaN(value.getTime())) return

    // An instant given an explicit display context follows that context, not
    // the shell: the caller has already decided what it is showing.
    const shellMode = this.el.dataset.followShell === "true" ? liveMode() : null
    const mode = shellMode || this.el.dataset.mode || "local"

    if (mode === "company" || mode === "utc") {
      // The server already rendered both. Copy, never re-format.
      const text = mode === "company" ? this.el.dataset.textCompany : this.el.dataset.textUtc
      this.showServerText(text)
      return
    }

    const timeZone = Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC"
    let text

    try {
      text = formatLocal(value, timeZone, this.el.dataset.format, this.el.dataset.precision)
    } catch {
      // A browser that cannot format this zone falls back to the server's
      // stored-UTC text. Keeping whatever is there would leave the previous
      // mode's string under a control that now reports a different mode.
      this.showServerText(this.el.dataset.textUtc)
      return
    }

    if (this.el.textContent !== text) this.el.textContent = text
    this.el.dataset.timezone = timeZone
    this.el.title = `Rendered in ${timeZone}`
  },

  showServerText(text) {
    if (text && this.el.textContent !== text) this.el.textContent = text
    delete this.el.dataset.timezone
    this.el.removeAttribute("title")
  },
}

window.BilibiUI.registerHook('DateTime', DateTime)

})();
