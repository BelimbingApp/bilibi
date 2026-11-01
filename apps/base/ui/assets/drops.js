// Dragging a record onto a page that can act on it (issue 265; base/ui
// `src/drops.rs` renders both halves). A row carrying `data-drag-record`
// ("kind:id") is the source; with some rows ticked, dragging a ticked row takes
// every ticked row of its table. A page carrying `data-drop-zone` with
// `data-drop-accepts` and `data-drop-max` is a target: it lights up while a
// record it accepts is dragged anywhere in the workspace, and a drop raises
// the page's `drop_records` event with the references as "kind:id,kind:id".
// The package is only a request: the page's owner re-reads every record under
// the person's own scope (`bilibi_records::Records::receive`). A drag from
// another site carries none of the types below and is ignored.
//
// Tiles are same-origin frames, so the browser carries the drag between them,
// but a frame sees nothing until the pointer enters it. The source frame
// therefore tells the workspace what it carries (the follow channel's sibling
// in tiling.js, which relays it to every frame), so every accepting tile lights
// up at once. The drop itself never goes through that channel.
(() => {
  const RECORDS = 'application/x-bilibi-records';
  const KIND_TYPE = 'application/x-bilibi-kind.';
  const CARRYING = 'bilibi:workspace:drag';
  const KIND = /^[a-z][a-z0-9_-]*(\.[a-z0-9_-]+)*$/;
  const MAX_RECORDS = 100;
  const isRecordId = (id) =>
    typeof id === 'string' && id !== '' && id.length <= 64 && id !== '.' && id !== '..' && !/[/?#\\\s,:\x00-\x1f\x7f]/.test(id);

  // What is being dragged now, as this document knows it: the kinds and how
  // many records. Set by a drag from this document or by the workspace.
  let carried = {kinds: [], count: 0};

  const zones = () => [...document.querySelectorAll('[data-drop-zone]')];
  const accepts = (zone) => (zone.dataset.dropAccepts || '').split(' ').filter(Boolean);
  const takes = (zone, kinds, count) => {
    const max = Number.parseInt(zone.dataset.dropMax || '0', 10);
    const allowed = accepts(zone);
    return kinds.length > 0 && count > 0 && count <= max && kinds.every((kind) => allowed.includes(kind));
  };
  // The tile as the person sees it: the framed page content, or the zone.
  const surface = (zone) => document.getElementById('app-content') || zone;
  // A pointer over the page margins is still over the tile.
  const zoneAt = (target) => {
    const el = target instanceof Element ? target : null;
    return el?.closest('[data-drop-zone]') || document.querySelector('[data-drop-zone]');
  };

  function paint() {
    for (const zone of zones()) {
      const ready = takes(zone, carried.kinds, carried.count);
      const tile = surface(zone);
      if (ready) {
        tile.dataset.dropReady = 'true';
        tile.dataset.dropLabel = zone.dataset.dropLabel || '';
      } else {
        delete tile.dataset.dropReady;
        delete tile.dataset.dropOver;
        delete tile.dataset.dropLabel;
      }
    }
  }

  function carry(kinds, count) {
    carried = {kinds, count};
    paint();
  }

  const post = (kinds, count) => {
    if (window.parent === window) return;
    try {
      window.parent.postMessage({type: CARRYING, kinds, count}, location.origin);
    } catch {
      /* the workspace is gone */
    }
  };

  // The workspace's word on what some tile carries.
  window.addEventListener('message', (event) => {
    if (event.origin !== location.origin || (window.parent !== window && event.source !== window.parent)) return;
    const data = event.data;
    if (!data || data.type !== CARRYING || !Array.isArray(data.kinds)) return;
    const kinds = data.kinds.filter((kind) => typeof kind === 'string' && KIND.test(kind)).slice(0, 8);
    const count = Number.isInteger(data.count) && data.count >= 0 && data.count <= MAX_RECORDS ? data.count : 0;
    carry(kinds, kinds.length ? count : 0);
  });

  // ---- Source ---------------------------------------------------------------
  const parseRecord = (text) => {
    const at = (text || '').indexOf(':');
    if (at < 1) return null;
    const kind = text.slice(0, at);
    const id = text.slice(at + 1);
    return KIND.test(kind) && isRecordId(id) ? {kind, id} : null;
  };
  const ticked = (row) => row.querySelector('[role="checkbox"][data-op="row"][aria-checked="true"]') !== null;

  function dragged(row) {
    const rows = ticked(row)
      ? [...(row.closest('table')?.querySelectorAll('tr[data-drag-record]') || [])].filter(ticked)
      : [row];
    const records = [];
    for (const each of rows) {
      const record = parseRecord(each.dataset.dragRecord);
      if (record && !records.some((seen) => seen.kind === record.kind && seen.id === record.id)) records.push(record);
    }
    return records.slice(0, MAX_RECORDS);
  }

  document.addEventListener('dragstart', (event) => {
    const row = event.target instanceof Element ? event.target.closest('[data-drag-record]') : null;
    if (!row || !event.dataTransfer) return;
    const records = dragged(row);
    if (!records.length) return;
    const kinds = [...new Set(records.map((record) => record.kind))];
    event.dataTransfer.effectAllowed = 'copy';
    event.dataTransfer.setData(RECORDS, JSON.stringify(records));
    // The kinds are readable while the drag is over a target; the data is not.
    for (const kind of kinds) event.dataTransfer.setData(KIND_TYPE + kind, '1');
    carry(kinds, records.length);
    post(kinds, records.length);
  });

  document.addEventListener('dragend', () => {
    if (!carried.count) return;
    carry([], 0);
    post([], 0);
  });

  // ---- Target ---------------------------------------------------------------
  // The kinds a drag carries, read from the types the browser exposes before
  // the drop; none when it is not ours (a file, text, a link from elsewhere).
  const typedKinds = (dt) => {
    const types = [...(dt?.types || [])];
    if (!types.includes(RECORDS)) return [];
    return types.filter((type) => type.startsWith(KIND_TYPE)).map((type) => type.slice(KIND_TYPE.length));
  };
  const over = (zone, on) => {
    const tile = surface(zone);
    if (on) tile.dataset.dropOver = 'true';
    else delete tile.dataset.dropOver;
  };

  document.addEventListener('dragover', (event) => {
    const zone = zoneAt(event.target);
    if (!zone) return;
    const kinds = typedKinds(event.dataTransfer);
    // A count comes from the channel; a drag that began in a page that never
    // announced it (older markup) is still judged by its kinds alone.
    const count = carried.count || 1;
    if (!takes(zone, kinds, count)) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = 'copy';
    over(zone, true);
  });

  document.addEventListener('dragleave', (event) => {
    const zone = zoneAt(event.target);
    if (!zone) return;
    // Leaving the tile (to nothing, or to another document) ends the hover.
    if (!event.relatedTarget || !document.documentElement.contains(event.relatedTarget)) over(zone, false);
  });

  document.addEventListener('drop', (event) => {
    const zone = zoneAt(event.target);
    if (!zone) return;
    over(zone, false);
    if (!typedKinds(event.dataTransfer).length) return;
    event.preventDefault();
    let records;
    try {
      records = JSON.parse(event.dataTransfer.getData(RECORDS));
    } catch {
      return;
    }
    if (!Array.isArray(records)) return;
    const parsed = records.map((record) =>
      record && typeof record.kind === 'string' && typeof record.id === 'string' ? parseRecord(`${record.kind}:${record.id}`) : null);
    if (!parsed.length || parsed.some((record) => !record)) return;
    if (!takes(zone, [...new Set(parsed.map((record) => record.kind))], parsed.length)) return;
    zone.dispatchEvent(new CustomEvent('bilibi:event', {
      bubbles: true,
      detail: {event: 'drop_records', records: parsed.map((record) => `${record.kind}:${record.id}`).join(',')},
    }));
  });
})();
