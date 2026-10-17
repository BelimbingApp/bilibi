// The browser halves of timestamp display, adapted from Bilimbi
// apps/web/assets/js/date_time.js and browser_time_zone.js at the pinned
// source. Served inside /assets/hooks.js after ui.js.
//
// DateTime (every `<time data-hook="DateTime">` the shared templates render)
// keeps a rendered instant on the saved display mode. The server decides the
// `company` and `utc` texts and writes both onto the element, so those two
// modes are a copy, never a re-format; `local` is the one mode the server
// cannot decide, so it is formatted here in the reader's own locale. The shell
// publishes the live mode on #app-shell (`data-display-mode`), and one
// observer repaints every mounted instant when it changes.
//
// BrowserTimeZone reports the browser's own zone to the page that mounted it
// as the `browser_timezone` event, once on mount and again whenever the shell
// mode changes: under the local mode only the browser knows which calendar
// day a row is shown on, so a screen whose date filters bound displayed days
// mounts it on the region holding them and recomputes its range without a
// reload. The server validates the name and keeps bounding UTC when it cannot
// honor it. A host whose DOM can survive a server remount supplies a fresh
// data-timezone-request per mount, then clears it after accepting a report.
(() => {
  const SHELL = "#app-shell"
  const shell = () => document.querySelector(SHELL)
  const liveMode = () => shell()?.dataset.displayMode || null
  const browserZone = () => Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC"

  // One observer for the whole page, armed on the first mount and re-armed if
  // the shell element is ever replaced rather than patched.
  const watchers = new Set()
  let observer = null
  let observed = null
  const observeShell = () => {
    const el = shell()
    if (!el || observed === el) return
    observer?.disconnect()
    observer = new MutationObserver(() => {
      for (const watcher of watchers) watcher.modeChanged()
    })
    observer.observe(el, {attributes: true, attributeFilter: ["data-display-mode"]})
    observed = el
  }
  const watch = (hook) => {
    watchers.add(hook)
    observeShell()
  }
  const unwatch = (hook) => {
    watchers.delete(hook)
    // With nothing left to repaint, stop holding a detached shell.
    if (watchers.size === 0) {
      observer?.disconnect()
      observer = null
      observed = null
    }
  }

  // `local` renders the instant the way the reader's own device would: their
  // locale orders the fields and chooses the hour cycle. The zone is named on
  // `datetime` only, where the label qualifies a full date and time.
  const formatLocal = (value, timeZone, format, precision) => {
    const options = {timeZone}
    if (format !== "time") Object.assign(options, {day: "2-digit", month: "2-digit", year: "numeric"})
    if (format !== "date") {
      Object.assign(options, {hour: "2-digit", minute: "2-digit"})
      if (precision === "second") options.second = "2-digit"
    }
    if (format === "datetime") options.timeZoneName = "short"
    return new Intl.DateTimeFormat(undefined, options).format(value)
  }

  // Every write compares first: ui.js reports a text write inside a hooked
  // element as `updated`, and an unconditional write would loop.
  const setText = (el, text) => {
    if (text != null && el.textContent !== text) el.textContent = text
  }

  const DateTime = {
    mounted() {
      watch(this)
      this.render()
    },
    updated() {
      this.render()
    },
    destroyed() {
      unwatch(this)
    },
    modeChanged() {
      this.render()
    },
    render() {
      const value = new Date(this.el.dateTime)
      if (Number.isNaN(value.getTime())) return
      // An instant given an explicit display context follows that context,
      // not the shell: the caller has already decided what it is showing.
      const shellMode = this.el.dataset.followShell === "true" ? liveMode() : null
      const mode = shellMode || this.el.dataset.mode || "local"
      if (mode === "company" || mode === "utc") {
        this.showServerText(mode === "company" ? this.el.dataset.textCompany : this.el.dataset.textUtc)
        return
      }
      const timeZone = browserZone()
      let text
      try {
        text = formatLocal(value, timeZone, this.el.dataset.format, this.el.dataset.precision)
      } catch {
        // A browser that cannot format this zone falls back to the server's
        // stored-UTC text rather than leaving the previous mode's string.
        this.showServerText(this.el.dataset.textUtc)
        return
      }
      setText(this.el, text)
      if (this.el.dataset.timezone !== timeZone) this.el.dataset.timezone = timeZone
      const title = `Rendered in ${timeZone}`
      if (this.el.getAttribute("title") !== title) this.el.setAttribute("title", title)
    },
    showServerText(text) {
      setText(this.el, text)
      if ("timezone" in this.el.dataset) delete this.el.dataset.timezone
      if (this.el.hasAttribute("title")) this.el.removeAttribute("title")
    },
  }

  const BrowserTimeZone = {
    mounted() {
      watch(this)
      this.report()
    },
    updated() {
      const request = this.el.dataset.timezoneRequest
      if (request && request !== this.reportedRequest) this.report()
    },
    destroyed() {
      unwatch(this)
    },
    // A saved top-bar mode changes displayed calendar days as well as labels,
    // so the host recomputes its range from the freshly authorized preference.
    modeChanged() {
      this.report()
    },
    report() {
      this.reportedRequest = this.el.dataset.timezoneRequest
      this.emit("browser_timezone", {timezone: browserZone()})
    },
  }

  const registry = window.BilibiUI
  if (!registry) throw new Error("datetime.js needs ui.js loaded first")
  registry.registerHook("DateTime", DateTime)
  registry.registerHook("BrowserTimeZone", BrowserTimeZone)
})()
