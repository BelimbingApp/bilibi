// The browser half of the shared status board (templates/board.html,
// src/board.rs). Served inside /assets/hooks.js after ui.js.
//
// The hook never writes and never moves a card itself: a drop only reports
// `board_move` (id, version, from, to) and the owner answers, so a refused or
// stale move leaves the card where the server drew it. A card is draggable
// only when the server gave it targets; only those columns accept the drop.
// Drag is off on coarse pointers (touch); "Move…" is the path there, and for
// the keyboard.
(() => {
  const CARD = "[data-board-card]"
  const COLUMN = "[data-board-column]"
  const OVER = ["bg-brand-surface", "ring-2", "ring-brand-strong/40"]
  const coarse = () => matchMedia("(pointer: coarse)").matches

  const Board = {
    mounted() {
      this.drag = null
      this.over = null

      this.accepts = (column, card) => {
        if (!column || !column.hasAttribute("data-accepts")) return false
        if (column.dataset.key === card.dataset.column) return false
        return (card.dataset.targets || "").split(",").includes(column.dataset.key)
      }
      this.clear = () => {
        if (!this.over) return
        this.over.classList.remove(...OVER)
        this.over = null
      }
      this.onDragStart = event => {
        const card = event.target instanceof Element ? event.target.closest(CARD) : null
        if (!card || !this.el.contains(card)) return
        if (!card.hasAttribute("data-movable") || coarse()) {
          event.preventDefault()
          return
        }
        this.drag = card
        event.dataTransfer.effectAllowed = "move"
        event.dataTransfer.setData("text/plain", card.dataset.cardId)
        card.classList.add("opacity-40")
      }
      this.onDragOver = event => {
        if (!this.drag) return
        const column = event.target instanceof Element ? event.target.closest(COLUMN) : null
        if (!this.accepts(column, this.drag)) {
          this.clear()
          return
        }
        event.preventDefault()
        event.dataTransfer.dropEffect = "move"
        if (this.over !== column) {
          this.clear()
          column.classList.add(...OVER)
          this.over = column
        }
      }
      this.onDrop = event => {
        if (!this.drag) return
        const column = event.target instanceof Element ? event.target.closest(COLUMN) : null
        if (!this.accepts(column, this.drag)) return
        event.preventDefault()
        this.emit("board_move", {
          id: this.drag.dataset.cardId,
          version: this.drag.dataset.version,
          from: this.drag.dataset.column,
          to: column.dataset.key,
        })
      }
      this.onDragEnd = () => {
        if (!this.drag) return
        this.drag.classList.remove("opacity-40")
        this.drag = null
        this.clear()
      }
      this.el.addEventListener("dragstart", this.onDragStart)
      this.el.addEventListener("dragover", this.onDragOver)
      this.el.addEventListener("drop", this.onDrop)
      this.el.addEventListener("dragend", this.onDragEnd)
    },

    destroyed() {
      this.clear?.()
      this.el.removeEventListener("dragstart", this.onDragStart)
      this.el.removeEventListener("dragover", this.onDragOver)
      this.el.removeEventListener("drop", this.onDrop)
      this.el.removeEventListener("dragend", this.onDragEnd)
    },
  }

  const registry = window.BilibiUI
  if (!registry) throw new Error("board.js needs ui.js loaded first")
  registry.registerHook("Board", Board)
})()
