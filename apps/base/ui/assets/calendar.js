// The browser half of the shared calendar (templates/calendar.html,
// src/calendar.rs). Served inside /assets/hooks.js after ui.js.
//
// The hook never writes and never moves a record itself: a drop or a resize
// only reports `calendar_move` (id, version, the wall-clock date and minute the
// item would start at, and its length) and the owner answers. A refused or
// stale move therefore leaves the item exactly where the server drew it. A
// ghost block shows where a drop would land; it is the only element the hook
// adds, and it is removed when the gesture ends.
//
// Drag is off on coarse pointers (touch); "Move…" is the path there, and for
// the keyboard. The month grid moves focus between days with the arrow keys.
(() => {
  const SNAP_DEFAULT = 15
  const ROW_MINUTES = 5
  const DAY_MINUTES = 24 * 60
  const ITEM = "[data-cal-item]"
  const TARGET = "[data-cal-column], [data-cal-cell]"
  const GHOST = "pointer-events-none z-10 col-span-full rounded-sm border-2 border-dashed border-brand-strong bg-brand-surface/50"
  const coarse = () => matchMedia("(pointer: coarse)").matches
  const number = (value, fallback = 0) => {
    const parsed = Number.parseInt(value, 10)
    return Number.isFinite(parsed) ? parsed : fallback
  }

  const Calendar = {
    mounted() {
      this.drag = null
      this.resize = null
      this.ghost = null
      this.snap = () => number(this.el.dataset.snap, SNAP_DEFAULT) || SNAP_DEFAULT

      // The minute of the day under the pointer in a time column, snapped.
      this.minuteAt = (column, clientY, round = Math.floor) => {
        const box = column.getBoundingClientRect()
        const fraction = Math.min(Math.max((clientY - box.top) / box.height, 0), 1)
        const snap = this.snap()
        return Math.min(round((fraction * DAY_MINUTES) / snap) * snap, DAY_MINUTES - snap)
      }

      this.showGhost = (column, minute, length) => {
        if (!column.matches("[data-cal-column]")) return this.clearGhost()
        if (!this.ghost) this.ghost = document.createElement("div")
        const first = Math.floor(minute / ROW_MINUTES) + 1
        const rows = Math.max(1, Math.ceil(Math.min(length, DAY_MINUTES - minute) / ROW_MINUTES))
        const className = `${GHOST} row-start-${first} row-span-${rows}`
        if (this.ghost.className !== className) this.ghost.className = className
        this.ghost.setAttribute("aria-hidden", "true")
        if (this.ghost.parentNode !== column) column.appendChild(this.ghost)
      }
      this.clearGhost = () => {
        this.ghost?.remove()
        this.ghost = null
        this.el.querySelectorAll("[data-cal-over]").forEach(cell => {
          cell.classList.remove("bg-brand-surface")
          cell.removeAttribute("data-cal-over")
        })
      }
      this.markCell = cell => {
        if (cell.hasAttribute("data-cal-over")) return
        this.el.querySelectorAll("[data-cal-over]").forEach(other => {
          other.classList.remove("bg-brand-surface")
          other.removeAttribute("data-cal-over")
        })
        cell.classList.add("bg-brand-surface")
        cell.setAttribute("data-cal-over", "")
      }

      // Where a drag over `target` would put the item.
      this.landing = (target, item, clientY) => {
        const lane = target.dataset.lane || ""
        const allDay = item.hasAttribute("data-all-day")
        if (target.matches("[data-cal-column]") && !allDay) {
          const minute = Math.max(0, this.minuteAt(target, clientY) - this.drag.grab)
          return {date: target.dataset.date, minute, lane, column: target}
        }
        return {date: target.dataset.date, minute: number(item.dataset.startMinute), lane, column: null}
      }

      this.report = (item, place, length) => {
        const allDay = item.hasAttribute("data-all-day")
        const payload = {
          id: item.dataset.itemId,
          version: item.dataset.version,
          date: place.date,
          lane: place.lane,
        }
        if (allDay) {
          payload.all_day = "true"
          payload.days = number(item.dataset.length, 1)
        } else {
          payload.minute = place.minute
          payload.length = length
        }
        this.emit("calendar_move", payload)
      }

      this.onDragStart = event => {
        const item = event.target instanceof Element ? event.target.closest(ITEM) : null
        if (!item || !item.hasAttribute("data-movable") || !this.el.contains(item) || coarse()) {
          if (item) event.preventDefault()
          return
        }
        const column = item.closest("[data-cal-column]")
        const snap = this.snap()
        const grab = column
          ? Math.round(((event.clientY - item.getBoundingClientRect().top) / column.getBoundingClientRect().height * DAY_MINUTES) / snap) * snap
          : 0
        this.drag = {item, grab: Math.max(0, grab)}
        event.dataTransfer.effectAllowed = "move"
        event.dataTransfer.setData("text/plain", item.dataset.itemId)
        item.classList.add("opacity-40")
      }
      this.onDragOver = event => {
        if (!this.drag) return
        const target = event.target instanceof Element ? event.target.closest(TARGET) : null
        if (!target || !this.el.contains(target)) return this.clearGhost()
        event.preventDefault()
        event.dataTransfer.dropEffect = "move"
        const place = this.landing(target, this.drag.item, event.clientY)
        if (place.column) {
          this.el.querySelectorAll("[data-cal-over]").forEach(cell => {
            cell.classList.remove("bg-brand-surface")
            cell.removeAttribute("data-cal-over")
          })
          this.showGhost(place.column, place.minute, number(this.drag.item.dataset.length, this.snap()))
        } else {
          this.ghost?.remove()
          this.ghost = null
          this.markCell(target)
        }
      }
      this.onDrop = event => {
        if (!this.drag) return
        const target = event.target instanceof Element ? event.target.closest(TARGET) : null
        if (!target || !this.el.contains(target)) return
        event.preventDefault()
        const item = this.drag.item
        const place = this.landing(target, item, event.clientY)
        const same = place.date === item.dataset.startDate
          && place.minute === number(item.dataset.startMinute)
          && (place.lane === "" || place.lane === item.dataset.lane)
        if (!same) this.report(item, place, number(item.dataset.length))
      }
      this.onDragEnd = () => {
        if (!this.drag) return
        this.drag.item.classList.remove("opacity-40")
        this.drag = null
        this.clearGhost()
      }

      // Resize: the bottom edge of a timed item, by pointer.
      this.onPointerDown = event => {
        const handle = event.target instanceof Element ? event.target.closest("[data-cal-resize]") : null
        if (!handle || event.button !== 0 || coarse()) return
        const item = handle.closest(ITEM)
        const column = item?.closest("[data-cal-column]")
        if (!item || !column || column.dataset.date !== item.dataset.startDate) return
        event.preventDefault()
        try { handle.setPointerCapture(event.pointerId) } catch { /* a synthetic or ended pointer */ }
        this.resize = {item, column, handle, pointer: event.pointerId, length: number(item.dataset.length)}
      }
      this.onPointerMove = event => {
        const state = this.resize
        if (!state || event.pointerId !== state.pointer) return
        const start = number(state.item.dataset.startMinute)
        const snap = this.snap()
        const end = Math.max(start + snap, this.minuteAt(state.column, event.clientY, Math.round))
        state.length = Math.min(end, DAY_MINUTES) - start
        this.showGhost(state.column, start, state.length)
      }
      this.finishResize = (event, apply) => {
        const state = this.resize
        if (!state || event.pointerId !== state.pointer) return
        this.resize = null
        try { state.handle.releasePointerCapture(state.pointer) } catch { /* already released */ }
        this.clearGhost()
        if (apply && state.length !== number(state.item.dataset.length)) {
          this.report(state.item, {date: state.item.dataset.startDate, minute: number(state.item.dataset.startMinute), lane: state.item.dataset.lane || ""}, state.length)
        }
      }
      this.onPointerUp = event => this.finishResize(event, true)
      this.onPointerCancel = event => this.finishResize(event, false)

      // The month grid: arrow keys move between days (a grid with one tab stop).
      this.onKeyDown = event => {
        const cell = event.target instanceof Element && event.target.matches("[role=gridcell]") ? event.target : null
        if (!cell || event.altKey || event.ctrlKey || event.metaKey) return
        const cells = [...this.el.querySelectorAll("[role=gridcell]")]
        const index = cells.indexOf(cell)
        const step = {ArrowLeft: -1, ArrowRight: 1, ArrowUp: -7, ArrowDown: 7}[event.key]
        let next = index
        if (step !== undefined) next = index + step
        else if (event.key === "Home") next = index - (index % 7)
        else if (event.key === "End") next = index - (index % 7) + 6
        else return
        event.preventDefault()
        const target = cells[Math.min(Math.max(next, 0), cells.length - 1)]
        if (!target || target === cell) return
        cell.tabIndex = -1
        target.tabIndex = 0
        target.focus()
      }

      this.el.addEventListener("dragstart", this.onDragStart)
      this.el.addEventListener("dragover", this.onDragOver)
      this.el.addEventListener("drop", this.onDrop)
      this.el.addEventListener("dragend", this.onDragEnd)
      this.el.addEventListener("pointerdown", this.onPointerDown)
      this.el.addEventListener("pointermove", this.onPointerMove)
      this.el.addEventListener("pointerup", this.onPointerUp)
      this.el.addEventListener("pointercancel", this.onPointerCancel)
      this.el.addEventListener("keydown", this.onKeyDown)
    },

    destroyed() {
      this.clearGhost()
      this.el.removeEventListener("dragstart", this.onDragStart)
      this.el.removeEventListener("dragover", this.onDragOver)
      this.el.removeEventListener("drop", this.onDrop)
      this.el.removeEventListener("dragend", this.onDragEnd)
      this.el.removeEventListener("pointerdown", this.onPointerDown)
      this.el.removeEventListener("pointermove", this.onPointerMove)
      this.el.removeEventListener("pointerup", this.onPointerUp)
      this.el.removeEventListener("pointercancel", this.onPointerCancel)
      this.el.removeEventListener("keydown", this.onKeyDown)
    },
  }

  const registry = window.BilibiUI
  if (!registry) throw new Error("calendar.js needs ui.js loaded first")
  registry.registerHook("Calendar", Calendar)
})()
