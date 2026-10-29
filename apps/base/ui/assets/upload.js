// Browser half of the live file input (templates/forms.html `file_input`,
// src/upload.rs): Bilimbi's Phoenix.LiveFileUpload hook and LiveUploader,
// ported to the Bilibi transport. The server owns the entry list, the rules
// (accept, max entries, max file size) and every byte received; this hook owns
// only the File objects the person chose and the stream that carries them.
//
//   * Choosing files announces them through the transport as an `upload`
//     intent (BilibiLive.send). The reply names, per file, the id to stream it
//     under and the offset to start at, or the error it was refused with; the
//     server then re-renders the owner, so the rows appear from its state.
//   * Accepted files are streamed as binary frames of `u32 id, u32 offset,
//     bytes`, one file at a time, one chunk in flight: the next chunk goes
//     when the server acknowledges the last (a `chunk` message, relayed here
//     as the window event `bilibi:upload`). The hook writes the row's
//     percentage between the server's own renders; a patch rewrites it from
//     the server's count, which is the truth.
//   * Remove sends `upload_cancel`; the server drops the entry and re-renders.
//   * A submit while a file is still arriving is held and re-raised once
//     every file has arrived, as LiveView waits for its uploads before the
//     submit event. The owner consumes only completed files, so nothing
//     half-received can reach it either way.
//   * A dropped socket ends every stream: the remounted state holds no bytes,
//     and the rows it renders say so.
//   * A resumable input (data-resumable; no Bilimbi counterpart) announces
//     each file's SHA-256, and the server stages its bytes on disk under it.
//     The hook keeps the files the server still listed when the socket
//     dropped and, once the page is remounted (`bilibi:remounted`), announces
//     them again: the server answers with the bytes it already holds, and the
//     stream carries on from there.
(() => {
  'use strict';
  let refs = 0;
  const HEADER = 8;
  const SLICE = 4 * 1024 * 1024;

  function ownerTarget(el) {
    const owner = el.closest('[data-target]');
    return owner ? owner.dataset.target : 'page';
  }

  // SHA-256 of a File, read in slices: SubtleCrypto cannot hash in parts,
  // and reading a large file whole would hold all of it in memory.
  const K = new Uint32Array([
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
  ]);
  async function sha256(file) {
    const h = new Uint32Array([0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19]);
    const w = new Uint32Array(64);
    const blocks = (view, end) => {
      for (let offset = 0; offset + 64 <= end; offset += 64) {
        for (let i = 0; i < 16; i++) w[i] = view.getUint32(offset + i * 4);
        for (let i = 16; i < 64; i++) {
          const x = w[i - 15], y = w[i - 2];
          const s0 = ((x >>> 7) | (x << 25)) ^ ((x >>> 18) | (x << 14)) ^ (x >>> 3);
          const s1 = ((y >>> 17) | (y << 15)) ^ ((y >>> 19) | (y << 13)) ^ (y >>> 10);
          w[i] = (w[i - 16] + s0 + w[i - 7] + s1) | 0;
        }
        let a = h[0], b = h[1], c = h[2], d = h[3], e = h[4], f = h[5], g = h[6], k = h[7];
        for (let i = 0; i < 64; i++) {
          const t1 = (k + (((e >>> 6) | (e << 26)) ^ ((e >>> 11) | (e << 21)) ^ ((e >>> 25) | (e << 7))) + ((e & f) ^ (~e & g)) + K[i] + w[i]) | 0;
          const t2 = ((((a >>> 2) | (a << 30)) ^ ((a >>> 13) | (a << 19)) ^ ((a >>> 22) | (a << 10))) + ((a & b) ^ (a & c) ^ (b & c))) | 0;
          k = g; g = f; f = e; e = (d + t1) | 0; d = c; c = b; b = a; a = (t1 + t2) | 0;
        }
        h[0] += a; h[1] += b; h[2] += c; h[3] += d; h[4] += e; h[5] += f; h[6] += g; h[7] += k;
      }
    };
    let position = 0;
    let tail = new Uint8Array(0);
    while (position < file.size) {
      const bytes = new Uint8Array(await file.slice(position, position + SLICE).arrayBuffer());
      position += bytes.length;
      if (!bytes.length) throw new Error('file shrank');
      const whole = bytes.length - (bytes.length % 64);
      blocks(new DataView(bytes.buffer), whole);
      tail = bytes.slice(whole);
    }
    // The last block: what is left, 0x80, zeros, then the length in bits.
    const last = new Uint8Array(tail.length < 56 ? 64 : 128);
    last.set(tail);
    last[tail.length] = 0x80;
    const view = new DataView(last.buffer);
    view.setUint32(last.length - 8, Math.floor(file.size / 0x20000000));
    view.setUint32(last.length - 4, (file.size * 8) >>> 0);
    blocks(view, last.length);
    return [...h].map((word) => word.toString(16).padStart(8, '0')).join('');
  }

  const FileUpload = {
    mounted() {
      this.pending = new Map(); // id -> {file, offset, size, chunk, inflight}
      this.files = new Map(); // id -> {file, sha256}: resumable files the server accepted
      this.carried = []; // resumable files to announce again after a remount
      this.hashing = 0; // resumable choices still being hashed
      this.choices = 0; // the latest choice, so a slower hash cannot overtake it
      this.announced = 0; // choices the server has not yet answered
      this.generation = 0;
      this.resume = null;
      this.resumable = this.el.hasAttribute('data-resumable');
      this.wrapper = this.el.closest('[data-file-input]') || this.el.parentElement;
      this.onChange = () => this.select([...this.el.files]);
      this.onAck = (event) => this.acked(event.detail || {});
      this.onDrop = () => this.reset();
      this.onRemount = () => this.carry();
      this.onClick = (event) => {
        const button = event.target.closest('button[name="upload-cancel"]');
        if (!button || !this.wrapper.contains(button) || button.disabled) return;
        this.cancel(Number(button.value), button);
      };
      this.onSubmit = (event) => {
        if (!this.busy()) return;
        event.preventDefault();
        event.stopImmediatePropagation();
        this.resume = {submitter: event.submitter || null};
        this.el.form.setAttribute('aria-busy', 'true');
      };
      this.el.addEventListener('change', this.onChange);
      this.wrapper.addEventListener('click', this.onClick);
      window.addEventListener('bilibi:upload', this.onAck);
      window.addEventListener('bilibi:disconnected', this.onDrop);
      window.addEventListener('bilibi:remounted', this.onRemount);
      this.release = null;
      this.form = this.el.form;
      if (this.form) this.form.addEventListener('submit', this.onSubmit, true);
    },

    destroyed() {
      this.el.removeEventListener('change', this.onChange);
      this.wrapper.removeEventListener('click', this.onClick);
      window.removeEventListener('bilibi:upload', this.onAck);
      window.removeEventListener('bilibi:disconnected', this.onDrop);
      window.removeEventListener('bilibi:remounted', this.onRemount);
      if (this.release) this.release();
      if (this.form) this.form.removeEventListener('submit', this.onSubmit, true);
      this.pending.clear();
      this.files.clear();
    },

    select(files) {
      if (!files.length || !window.BilibiLive) return;
      // The input forgets its selection so the same file can be chosen again
      // after a Remove; the rows the server renders are the record of it.
      this.el.value = '';
      if (!this.resumable) {
        this.announce(files.map((file) => ({file})));
        return;
      }
      // Hashing takes a moment for a large file; a submit meanwhile waits,
      // and a drop meanwhile only delays the announcement, which the
      // transport sends after the remount. A file over the input's limit is
      // not hashed: the server refuses it by its size.
      const limit = Number(this.el.dataset.maxFileSize) || Infinity;
      const choice = ++this.choices;
      this.hashing += 1;
      Promise.all(files.map((file) => (file.size > limit ? Promise.reject() : sha256(file)).then((digest) => ({file, sha256: digest}), () => ({file})))).then((chosen) => {
        this.hashing -= 1;
        // A one-file input takes the latest choice, as the browser control does.
        if (Number(this.el.dataset.maxEntries) === 1 && choice !== this.choices) { this.settle(); return; }
        this.announce(chosen);
      });
    },

    // Announces files ({file, sha256?}) and streams each one accepted from
    // the offset the server names.
    announce(chosen) {
      // A development rebuild keeps the page while files wait to carry on,
      // through every failed reconnect too (live.js loads after the hooks,
      // so this registers on first use).
      if (this.resumable && !this.release) this.release = window.BilibiLive.hold(() => this.hashing > 0 || this.carried.length > 0 || this.listed().length > 0);
      const held = new Map();
      const digests = {};
      const entries = chosen.map(({file, sha256: digest}) => {
        const ref = String(++refs);
        held.set(ref, {file, sha256: digest});
        if (digest) digests[ref] = digest;
        return {ref, name: file.name || ref, size: file.size, content_type: file.type || ''};
      });
      const generation = this.generation;
      const target = ownerTarget(this.el);
      const intent = {type: 'upload', target, upload: this.el.dataset.upload, entries};
      if (Object.keys(digests).length) intent.sha256 = digests;
      this.announced += 1;
      window.BilibiLive.send(intent, {
        source: this.el,
        onReply: (reply) => {
          if (generation === this.generation) this.announced = Math.max(0, this.announced - 1);
          if (reply.type !== 'upload') { this.settle(); return; }
          for (const registration of reply.entries || []) {
            if (registration.id == null) continue;
            const chosenFile = held.get(registration.ref);
            // Files announced before a drop are gone with the old state; a
            // resumable one sent after it is held by the new state, so it goes on.
            if (generation !== this.generation && !chosenFile?.sha256) {
              window.BilibiLive.send({type: 'upload_cancel', target, id: registration.id});
              continue;
            }
            if (!chosenFile) continue;
            if (chosenFile.sha256) this.files.set(registration.id, chosenFile);
            if (registration.done) continue;
            const {file} = chosenFile;
            this.pending.set(registration.id, {file, offset: registration.received || 0, size: file.size, chunk: registration.chunk || 65536, inflight: false});
            this.paint(registration.id, registration.received || 0, file.size);
          }
          this.pump();
          this.settle();
        },
      });
    },

    // One file at a time, one chunk in flight, in the order the files were
    // chosen; the server processes a socket sequentially either way.
    pump() {
      if ([...this.pending.values()].some((entry) => entry.inflight)) return;
      const next = this.pending.entries().next();
      if (next.done) return;
      const [id, entry] = next.value;
      entry.inflight = true;
      const end = Math.min(entry.offset + entry.chunk, entry.size);
      entry.file.slice(entry.offset, end).arrayBuffer().then((buffer) => {
        if (!this.pending.has(id) || this.pending.get(id) !== entry) return;
        const frame = new Uint8Array(HEADER + buffer.byteLength);
        const view = new DataView(frame.buffer);
        view.setUint32(0, id);
        view.setUint32(4, entry.offset);
        frame.set(new Uint8Array(buffer), HEADER);
        if (!window.BilibiLive.sendBytes(frame)) this.reset();
      }, () => {
        // The file changed or vanished under the person; the server marks the
        // entry interrupted when the stream stops, and the row says so.
        this.pending.delete(id);
        this.files.delete(id);
        this.pump();
        this.settle();
      });
    },

    acked(detail) {
      const entry = this.pending.get(detail.id);
      if (!entry) return;
      if (detail.error) {
        this.pending.delete(detail.id);
        this.files.delete(detail.id);
      } else {
        entry.offset = detail.received;
        entry.inflight = false;
        this.paint(detail.id, detail.received, entry.size);
        if (detail.done) this.pending.delete(detail.id);
      }
      this.pump();
      this.settle();
    },

    paint(id, received, size) {
      const row = this.wrapper.querySelector(`[data-upload-entry="${id}"] [data-upload-progress]`);
      if (!row) return;
      const text = `${size ? Math.min(100, Math.floor(received * 100 / size)) : 100}%`;
      if (row.textContent !== text) row.textContent = text;
    },

    cancel(id, button) {
      this.pending.delete(id);
      this.files.delete(id);
      window.BilibiLive.send({type: 'upload_cancel', target: ownerTarget(this.el), id}, {source: button});
      this.pump();
      this.settle();
    },

    // Files are busy from the moment they are chosen: a submit raised before
    // the server has even answered the choice waits like one raised mid-stream,
    // and so does one raised while resumable files wait for the remount.
    busy() {
      return this.pending.size > 0 || this.announced > 0 || this.hashing > 0 || this.carried.length > 0;
    },

    // The held submit goes once every stream has ended, whatever ended it.
    settle() {
      if (!this.resume || this.busy()) return;
      const {submitter} = this.resume;
      this.resume = null;
      if (!this.form) return;
      this.form.removeAttribute('aria-busy');
      if (submitter && submitter.isConnected && submitter.form === this.form) this.form.requestSubmit(submitter);
      else this.form.requestSubmit();
    },

    // The resumable files whose rows the server still renders: consumed,
    // removed and refused ones have left the list.
    listed() {
      return [...this.files].filter(([id]) => this.wrapper.querySelector(`[data-upload-entry="${id}"]`)).map(([, file]) => file);
    },

    // The socket dropped: the listed resumable files wait for the remount,
    // and every stream ends.
    reset() {
      // A failed reconnect drops again; keep what the first drop carried.
      const listed = this.listed();
      if (listed.length) this.carried = listed;
      this.files.clear();
      this.generation += 1;
      this.pending.clear();
      this.announced = 0;
      // A held submit waits for files that carry on; otherwise it is gone.
      if (this.busy()) return;
      this.resume = null;
      if (this.form) this.form.removeAttribute('aria-busy');
    },

    // The page was remounted on a new socket: announce the carried files
    // again, right behind the mount, so the server finds their staged bytes.
    carry() {
      const carried = this.carried;
      this.carried = [];
      if (carried.length && this.el.isConnected && !this.el.disabled) this.announce(carried);
      else this.settle();
    },
  };

  window.BilibiUI.registerHook('FileUpload', FileUpload);
})();
