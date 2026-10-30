// Specimen only: stands in for the mounted owner and its bounded search results.
(() => {
  const root = document.getElementById("specimen-search-multi-wrapper")
  if (!root) return
  const input = root.querySelector('[role="combobox"]')
  const results = [...root.querySelectorAll('[role="option"]:not([data-create])')]
  const selected = new Map([["1", "Acacia"]])
  const row = root.querySelector('.ui-multi-chips')
  const count = root.querySelector('#specimen-search-multi-count')
  const status = document.getElementById('specimen-search-multi-status')
  root.addEventListener('bilibi:event', event => {
    const {event: name, value, query} = event.detail
    if (name === 'sample-search') {
      root.dataset.query = query
      for (const option of results) {
        option.parentElement.hidden = !option.textContent.toLowerCase().includes(query.toLowerCase())
        const link = [...root.querySelectorAll('[data-result-link]')].find(item => item.dataset.resultLink === option.dataset.value)
        if (link) link.hidden = option.parentElement.hidden
      }
    } else if (name === 'sample-pick') {
      const option = results.find(item => item.dataset.value === value)
      if (!option || option.getAttribute('aria-disabled') === 'true') return
      if (selected.has(value)) selected.delete(value)
      else selected.set(value, option.firstChild.textContent.trim())
    } else if (name === 'sample-remove') selected.delete(value)
    else if (name === 'sample-create') status.textContent = `Create requested: ${query}`
    else return
    for (const chip of row.querySelectorAll('[data-selected-chip]')) chip.remove()
    for (const [id, label] of selected) {
      const chip = document.createElement('span')
      chip.dataset.selectedChip = ''
      chip.className = 'ui-multi-chip'
      const text = document.createElement('span')
      text.textContent = label
      const remove = document.createElement('button')
      remove.type = 'button'
      remove.className = 'ui-multi-remove'
      remove.dataset.remove = id
      remove.setAttribute('aria-label', `Remove ${label}`)
      remove.textContent = '×'
      const hidden = document.createElement('input')
      hidden.type = 'hidden'
      hidden.name = 'specimen[companies][]'
      hidden.value = id
      chip.append(text, remove, hidden)
      row.insertBefore(chip, row.querySelector('[data-chip-overflow]'))
    }
    count.textContent = `(${selected.size} selected)`
    for (const option of results) option.setAttribute('aria-selected', String(selected.has(option.dataset.value)))
  })
})()
